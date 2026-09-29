//! `post participant gc`: collect participant records that hold nothing.
//!
//! Every session that ever ran a hook minted a record; 54 of 1,799 ever held
//! state. Ids are deterministic, so an empty record can be deleted and the next
//! `bind` for its key mints it again under the same id. Two tiers:
//!
//! - **Tier 1, delete.** Not active, last seen more than 7 days ago (24 hours
//!   for an ephemeral `bind --new` record), a directory holding nothing but
//!   scaffolding, and nothing names the id. A tombstone in
//!   `participants/archived.jsonl` keeps the id occupied for its key.
//! - **Tier 2, archive.** Not active, last seen more than 30 days ago, with
//!   state, and no unread, pending, or held mail. The directory moves whole to
//!   `<root>/participants-archive/<id>/`; `bind` moves it back.
//!
//! Never touched: an active lease, a fresh watch heartbeat, a lineage's current
//! holder, a doorbell subscription, a sender of an outbound bridge letter that
//! is still in flight, or anyone with unread, pending, or held mail. Frozen
//! unread mail on a stale participant is retained, never rerouted.
//!
//! A dry run (the default) and an apply come from the same plan, so their
//! lists match; a second apply finds nothing left to do.

use super::inbox::visible_addresses_among;
use crate::command_result::CommandResult;
use crate::cursor_state::{eligibility, routing};
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::participant::{self, gc, Participant};
use serde::Serialize;
use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// Tier 1 for an ordinary record.
const TIER1_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Tier 1 for an ephemeral (`bind --new`) record.
const TIER1_EPHEMERAL_WINDOW: Duration = DAY;
/// Tier 2.
const TIER2_WINDOW: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Records handled per hold of the participants lock. A bind or a routing pass
/// that arrives mid-run waits for one batch, not the whole run.
const BATCH: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    Delete,
    Archive,
}

#[derive(Debug)]
pub(crate) struct Planned {
    participant: Participant,
    tier: Tier,
}

impl Planned {
    pub(crate) fn id(&self) -> &str {
        &self.participant.id
    }
}

#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub actions: Vec<Planned>,
    pub kept: BTreeMap<&'static str, usize>,
}

impl Plan {
    fn ids(&self, tier: Tier) -> Vec<String> {
        self.actions
            .iter()
            .filter(|planned| planned.tier == tier)
            .map(|planned| planned.id().to_owned())
            .collect()
    }
}

#[derive(Serialize)]
struct GcOutput {
    ok: bool,
    applied: bool,
    deleted: Vec<String>,
    archived: Vec<String>,
    kept: BTreeMap<String, usize>,
}

pub(super) fn run(context: &Context, apply: bool, pretty: bool) -> AppResult<CommandResult> {
    let now = SystemTime::now();
    let plan = plan(context, now)?;
    let (deleted, archived, kept) = if apply {
        let done = apply_plan(context, &plan, now, &mut |_| Ok(()))?;
        let mut kept = plan.kept.clone();
        // Planned, then found to hold something by the time its turn came.
        if done.skipped > 0 {
            *kept.entry("changed").or_default() += done.skipped;
        }
        (done.deleted, done.archived, kept)
    } else {
        (
            plan.ids(Tier::Delete),
            plan.ids(Tier::Archive),
            plan.kept.clone(),
        )
    };
    CommandResult::json(
        &GcOutput {
            ok: true,
            applied: apply,
            deleted,
            archived,
            kept: kept
                .into_iter()
                .map(|(reason, count)| (reason.to_owned(), count))
                .collect(),
        },
        pretty,
    )
}

/// Everything a pass learns about the host, each thing at most once.
///
/// The planner's `Host` is a snapshot: one read answers every candidate. An
/// apply builds a fresh `Host` per batch, after taking the participants lock,
/// and asks it about each candidate immediately before collecting it. What
/// anything that takes the lock can change (receipts, sends to a participant,
/// bridge imports, binds) is settled when the batch starts and cannot move
/// under it. What does not take the lock is read again per candidate: the
/// record, its heartbeat, its doorbell subscription, its own inbox, and (by
/// the directory's fingerprint) the workspace's inbox, where a letter can land
/// at any moment.
struct Host<'a> {
    context: &'a Context,
    now: SystemTime,
    /// Check a cached workspace answer against the inbox's fingerprint.
    revalidate: bool,
    index: OnceCell<routing::ReceivedIndex>,
    in_flight: OnceCell<HashSet<String>>,
    unrouted: std::cell::RefCell<BTreeMap<String, (Option<InboxStamp>, bool)>>,
}

/// A directory's modification time and entry count: changes whenever a letter
/// is added to it.
type InboxStamp = (SystemTime, usize);

fn inbox_stamp(inbox: &std::path::Path) -> Option<InboxStamp> {
    let modified = std::fs::metadata(inbox).ok()?.modified().ok()?;
    let entries = std::fs::read_dir(inbox).ok()?.count();
    Some((modified, entries))
}

/// Whether an answer cached against `seen` still holds. A plan trusts its
/// snapshot; an apply trusts the cache only while the inbox looks exactly as it
/// did. A stamp that cannot be taken is never proof that nothing changed (a
/// filesystem without modification times would otherwise never revalidate).
fn cache_still_valid(revalidate: bool, seen: Option<InboxStamp>, now: Option<InboxStamp>) -> bool {
    !revalidate || (now.is_some() && seen == now)
}

impl<'a> Host<'a> {
    fn new(context: &'a Context, now: SystemTime, revalidate: bool) -> Self {
        Self {
            context,
            now,
            revalidate,
            index: OnceCell::new(),
            in_flight: OnceCell::new(),
            unrouted: std::cell::RefCell::new(BTreeMap::new()),
        }
    }

    fn index(&self) -> &routing::ReceivedIndex {
        self.index
            .get_or_init(|| routing::ReceivedIndex::read(self.context))
    }

    /// Local senders of outbound bridge letters not yet received or rejected.
    fn in_flight_senders(&self) -> &HashSet<String> {
        self.in_flight
            .get_or_init(|| in_flight_senders(self.context))
    }

    /// Whether the workspace's canonical inbox holds a letter with no receipt.
    fn workspace_has_unrouted(&self, workspace: &str) -> bool {
        let address = participant::Address {
            kind: participant::AddressKind::Workspace,
            name: workspace.to_owned(),
        };
        let stamp = if self.revalidate {
            inbox_stamp(&routing::inbox_path(self.context, &address))
        } else {
            None
        };
        if let Some((seen, known)) = self.unrouted.borrow().get(workspace) {
            if cache_still_valid(self.revalidate, *seen, stamp) {
                return *known;
            }
        }
        // A store that cannot be read may hold anything: assume it does.
        let unrouted = routing::has_unrouted_mail(self.context, &address).unwrap_or(true);
        self.unrouted
            .borrow_mut()
            .insert(workspace.to_owned(), (stamp, unrouted));
        unrouted
    }
}

/// Decide what to collect. Reads only; changes nothing.
pub(crate) fn plan(context: &Context, now: SystemTime) -> AppResult<Plan> {
    let host = Host::new(context, now, false);
    let mut plan = Plan::default();
    for participant in participant::list(context)? {
        match classify(&host, &participant)? {
            Verdict::Keep(reason) => *plan.kept.entry(reason).or_default() += 1,
            Verdict::Collect(tier) => plan.actions.push(Planned { participant, tier }),
        }
    }
    Ok(plan)
}

enum Verdict {
    Keep(&'static str),
    Collect(Tier),
}

fn classify(host: &Host<'_>, participant: &Participant) -> AppResult<Verdict> {
    if participant.is_active(host.now) {
        return Ok(Verdict::Keep("active"));
    }
    let Some(age) = idle_for(participant, host.now) else {
        return Ok(Verdict::Keep("no_last_seen"));
    };
    let tier1_window = if participant.ephemeral {
        TIER1_EPHEMERAL_WINDOW
    } else {
        TIER1_WINDOW
    };
    if age <= tier1_window {
        return Ok(Verdict::Keep("recent"));
    }
    if heartbeat_is_fresh(participant, host.now, tier1_window) {
        return Ok(Verdict::Keep("live_watch"));
    }
    if participant.lineage.is_some() {
        return Ok(Verdict::Keep("lineage"));
    }
    if subscription_path(host.context, &participant.id).exists() {
        return Ok(Verdict::Keep("subscribed"));
    }
    if host.in_flight_senders().contains(&participant.id) {
        return Ok(Verdict::Keep("outbound_in_flight"));
    }
    if holds_only_scaffolding(participant) {
        if host.index().names(&participant.id) {
            return Ok(Verdict::Keep("named_by_receipt"));
        }
        if participant
            .workspace
            .as_deref()
            .is_some_and(|workspace| host.workspace_has_unrouted(workspace))
        {
            return Ok(Verdict::Keep("pending_mail"));
        }
        return Ok(Verdict::Collect(Tier::Delete));
    }
    if age <= TIER2_WINDOW {
        return Ok(Verdict::Keep("recent"));
    }
    if let Some(reason) = mail_reason(host, participant) {
        return Ok(Verdict::Keep(reason));
    }
    Ok(Verdict::Collect(Tier::Archive))
}

/// How long since the participant did anything, from `last_seen` (an ended
/// participant's `last_seen` is its end time), falling back to `created`.
fn idle_for(participant: &Participant, now: SystemTime) -> Option<Duration> {
    let seen = participant
        .last_seen
        .as_deref()
        .and_then(participant::parse_rfc3339)
        .or_else(|| participant::parse_sent_timestamp(&participant.created))?;
    Some(now.duration_since(seen).unwrap_or_default())
}

fn heartbeat_is_fresh(participant: &Participant, now: SystemTime, window: Duration) -> bool {
    crate::presence::participant_heartbeat_stamp(participant).is_some_and(|stamp| {
        let beat = SystemTime::UNIX_EPOCH + Duration::from_secs(stamp);
        now.duration_since(beat).map_or(true, |age| age <= window)
    })
}

/// A doorbell prefs file means someone configured this participant's doorbell.
fn subscription_path(context: &Context, id: &str) -> PathBuf {
    context
        .root
        .join("doorbell")
        .join("prefs")
        .join(format!("{id}.json"))
}

fn holds_only_scaffolding(participant: &Participant) -> bool {
    let Ok(entries) = std::fs::read_dir(&participant.dir) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return false;
        };
        if !gc::is_stateless_entry(&name, &entry.path()) {
            return false;
        }
    }
    true
}

/// Why this participant's archive would strand mail, if it would. Anything
/// this cannot read counts: a record is archived only when its state is
/// known to hold no mail.
fn mail_reason(host: &Host<'_>, participant: &Participant) -> Option<&'static str> {
    let received = match host.index().received(participant) {
        Ok(received) => received,
        Err(_) => return Some("unreadable_state"),
    };
    for address in visible_addresses_among(participant, received) {
        match eligibility::unread_mail_snapshot(host.context, participant, &address) {
            Ok(snapshot) if snapshot.items.is_empty() && snapshot.skipped_unreadable == 0 => {}
            Ok(_) => return Some("unread_mail"),
            Err(_) => return Some("unreadable_state"),
        }
        match routing::provisional_pending_for_quiet(host.context, participant, &address) {
            Ok(pending) if pending.is_empty() => {}
            Ok(_) => return Some("pending_mail"),
            Err(_) => return Some("unreadable_state"),
        }
        match routing::held_for(host.context, participant, &address) {
            Ok(held) if held.is_empty() => {}
            Ok(_) => return Some("pending_mail"),
            Err(_) => return Some("unreadable_state"),
        }
    }
    None
}

/// Participants that sent an outbound bridge letter the peer has neither
/// received nor rejected. Their records must stay so the receipt can name them.
fn in_flight_senders(context: &Context) -> HashSet<String> {
    let bridge = crate::bridge_topology::bridge_dir(context);
    let ids = |directory: &str| -> HashSet<String> {
        std::fs::read_dir(bridge.join(directory))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".json"))
                    .map(str::to_owned)
            })
            .collect()
    };
    let acked = ids("pmail-acked");
    ids("pmail-status")
        .into_iter()
        .chain(ids("pmail-published"))
        .filter(|id| !acked.contains(id))
        .filter_map(|id| {
            crate::mailbox::parse_mail(&context.root.join("archive").join(format!("{id}.mail")))
                .ok()
                .and_then(|mail| mail.envelope.from_participant)
        })
        .collect()
}

/// The named steps of collecting one participant, for fault injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The lock is held and a candidate is about to be re-checked.
    BeforeCheck,
    /// The tombstone is durable; the directory has not moved.
    AfterTombstone,
    /// The directory is out of the registry; the session index still names it.
    AfterMove,
}

pub(crate) struct Applied {
    pub deleted: Vec<String>,
    pub archived: Vec<String>,
    /// Planned candidates that were not still collectable when their turn came.
    pub skipped: usize,
}

/// Carry out a plan, one participant at a time, in small batches under the
/// participants lock. Each participant goes through the whole keep
/// classification again, under the lock and immediately before its own delete
/// or move, so anything that arrived since planning (mail, a receipt naming
/// it, a subscription, a heartbeat, activity) keeps it. A candidate that is
/// not still collectable in the tier that was planned is skipped and counted
/// as `changed`.
///
/// The writers that put something into a participant's directory take the same
/// lock, or check under it that the record is still there and bring it back
/// (`participant::revive_locked`), so nothing lands in a directory this run is
/// about to remove.
///
/// Order per participant, so a kill at any step leaves it whole or gone:
/// tombstone (tier 1) before the directory moves, the directory move as one
/// rename, the by-session entry removed last. A kill after the move leaves an
/// index entry `resolve` reads as never bound, and `bind` mints (or restores)
/// the same id.
pub(crate) fn apply_plan(
    context: &Context,
    plan: &Plan,
    now: SystemTime,
    hook: &mut dyn FnMut(Step) -> AppResult<()>,
) -> AppResult<Applied> {
    let mut applied = Applied {
        deleted: Vec::new(),
        archived: Vec::new(),
        skipped: 0,
    };
    for batch in plan.actions.chunks(BATCH) {
        let _lock = participant::lock(context)?;
        gc::sweep_leftovers(context);
        // A view taken now, with the lock held: nothing that takes the lock
        // can change what it reads until the batch is done.
        let host = Host::new(context, now, true);
        for planned in batch {
            hook(Step::BeforeCheck)?;
            if !still_collectable(&host, planned)? {
                applied.skipped += 1;
                continue;
            }
            let participant = &planned.participant;
            match planned.tier {
                Tier::Delete => {
                    gc::append_tombstone(context, &tombstone(participant, now)?)?;
                    hook(Step::AfterTombstone)?;
                    gc::delete_dir(context, &participant.id)?;
                }
                Tier::Archive => {
                    gc::archive_dir(context, &participant.id)?;
                }
            }
            hook(Step::AfterMove)?;
            gc::remove_index(
                context,
                &participant.harness,
                &participant.conversation_key_digest,
                &participant.id,
            )?;
            match planned.tier {
                Tier::Delete => applied.deleted.push(participant.id.clone()),
                Tier::Archive => applied.archived.push(participant.id.clone()),
            }
        }
    }
    Ok(applied)
}

fn tombstone(participant: &Participant, now: SystemTime) -> AppResult<gc::Tombstone> {
    Ok(gc::Tombstone {
        id: participant.id.clone(),
        harness: participant.harness.clone(),
        conversation_key_digest: participant.conversation_key_digest.clone(),
        created: participant.created.clone(),
        last_seen: participant.last_seen.clone(),
        workspace: participant.workspace.clone(),
        ephemeral: participant.ephemeral,
        workspace_path: participant.workspace_path.clone(),
        display_name: participant.display_name.clone(),
        lease_hours: Some(participant.lease_hours),
        reason: "stateless and idle".to_owned(),
        archived_at: participant::format_rfc3339(now)?,
    })
}

/// The plan's decision, made again under the lock: the record is the one that
/// was planned, and the full keep classification still collects it in the tier
/// that was planned.
fn still_collectable(host: &Host<'_>, planned: &Planned) -> AppResult<bool> {
    let Some(current) = participant::load(host.context, planned.id())? else {
        return Ok(false);
    };
    let before = &planned.participant;
    if current.last_seen != before.last_seen
        || current.ended_at != before.ended_at
        || current.lineage != before.lineage
    {
        return Ok(false);
    }
    Ok(matches!(
        classify(host, &current)?,
        Verdict::Collect(tier) if tier == planned.tier
    ))
}

/// Records for tests here and in the writers' tests (`send`): a participant
/// file, optional state, and the session index entry, written directly so a
/// test controls exactly how idle the record is.
#[cfg(test)]
pub(crate) mod test_seed {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::Path;

    pub(crate) fn digest_of(key: &str) -> String {
        format!("{:x}", Sha256::digest(key.as_bytes()))
    }

    pub(crate) fn seed(
        root: &Path,
        key: &str,
        last_seen: &str,
        with_state: bool,
    ) -> (String, String) {
        seed_in(root, key, last_seen, with_state, None)
    }

    pub(crate) fn seed_in(
        root: &Path,
        key: &str,
        last_seen: &str,
        with_state: bool,
        workspace: Option<&str>,
    ) -> (String, String) {
        let digest = digest_of(key);
        let id = format!("claude-{}", &digest[..8]);
        let dir = root.join("participants").join(&id);
        fs::create_dir_all(&dir).expect("participant dir");
        let mut record = serde_json::json!({
            "version": 1,
            "id": id,
            "harness": "claude",
            "conversation_key_digest": digest,
            "created": "2026-01-01 00:00:00 +0000",
            "last_seen": last_seen,
            "lease_hours": 24,
        });
        if let Some(workspace) = workspace {
            record["workspace"] = serde_json::json!(workspace);
        }
        fs::write(
            dir.join("participant.json"),
            serde_json::to_vec_pretty(&record).expect("json"),
        )
        .expect("record");
        if with_state {
            fs::write(
                dir.join("cursors.json"),
                b"{\"version\":2,\"mail\":{},\"channels\":{}}\n",
            )
            .expect("state");
        }
        let index = root.join("participants/by-session/claude");
        fs::create_dir_all(&index).expect("index dir");
        fs::write(index.join(&digest), format!("{id}\n")).expect("index entry");
        (id, digest)
    }

    pub(crate) fn days_ago(days: u32, now: SystemTime) -> String {
        participant::format_rfc3339(now - DAY * days).expect("timestamp")
    }
}

#[cfg(test)]
mod tests {
    use super::test_seed::{days_ago, seed, seed_in};
    use super::*;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;
    use std::path::Path;

    /// A kill at each named step leaves the participant either whole or wholly
    /// put away, and its session resolves to the same id either way: bound to
    /// the live record, restorable from the archive, or unbound and re-mintable
    /// (the tombstone keeps its id).
    #[test]
    fn a_kill_at_any_step_leaves_the_participant_whole_or_put_away() {
        for (with_state, fail_at) in [
            (false, Step::AfterTombstone),
            (false, Step::AfterMove),
            (true, Step::AfterMove),
        ] {
            let root = test_root("gc-crash");
            let context = Context {
                root: root.clone(),
                home: root.clone(),
            };
            let now = SystemTime::now();
            let (id, digest) = seed(&root, "crash-key", &days_ago(40, now), with_state);
            let plan = plan(&context, now).expect("plan");
            assert_eq!(plan.actions.len(), 1, "state={with_state}: planned");

            let killed = apply_plan(&context, &plan, now, &mut |step| {
                if step == fail_at {
                    Err(crate::error::AppError::invalid_argument("injected kill"))
                } else {
                    Ok(())
                }
            });
            assert!(killed.is_err(), "the injected kill surfaces");

            let live = root.join("participants").join(&id).join("participant.json");
            let resolved = participant::resolve_key(&context, "claude", &digest)
                .expect("a half-collected participant still resolves");
            match (with_state, fail_at) {
                (_, Step::BeforeCheck) => unreachable!("not a kill point in this test"),
                (false, Step::AfterTombstone) => {
                    assert!(live.exists(), "killed before the move: still live");
                    assert!(matches!(resolved, participant::KeyResolution::Live(_)));
                }
                (false, Step::AfterMove) => {
                    assert!(!live.exists(), "killed after the move: gone");
                    assert!(
                        matches!(resolved, participant::KeyResolution::Unbound),
                        "the leftover index entry reads as never bound, not as missing"
                    );
                    assert!(matches!(
                        gc::holder(&context, &id).expect("holder"),
                        gc::Holder::Tombstone { digest: held } if held == digest
                    ));
                }
                (true, _) => {
                    assert!(!live.exists(), "archived: out of the registry");
                    assert!(matches!(
                        resolved,
                        participant::KeyResolution::Archived { id: held } if held == id
                    ));
                    assert!(gc::archived_dir(&context, &id)
                        .join("cursors.json")
                        .exists());
                }
            }
            trash_test_root(&root);
        }
    }

    #[test]
    fn a_participant_that_woke_up_since_planning_is_left_alone() {
        let root = test_root("gc-woke");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let now = SystemTime::now();
        let (id, _) = seed(&root, "woke-key", &days_ago(40, now), false);
        let plan = plan(&context, now).expect("plan");
        assert_eq!(plan.actions.len(), 1);
        // The session touches its record between the plan and the apply.
        participant::touch(&context, &id).expect("touch");
        let applied = apply_plan(&context, &plan, now, &mut |_| Ok(())).expect("apply");
        assert!(applied.deleted.is_empty() && applied.archived.is_empty());
        assert!(root
            .join("participants")
            .join(&id)
            .join("participant.json")
            .exists());
        trash_test_root(&root);
    }

    fn context_at(root: &Path) -> Context {
        Context {
            root: root.to_path_buf(),
            home: root.to_path_buf(),
        }
    }

    /// Two registered rooms, and a rule that blocks `beta` from reaching
    /// `alpha`, so a letter from beta that lands in alpha is held.
    fn write_rooms_and_rules(root: &Path) {
        fs::write(
            root.join("rooms.json"),
            format!(
                r#"{{"alpha": "{0}/alpha", "beta": "{0}/beta", "gamma": "{0}/gamma"}}"#,
                root.display()
            ),
        )
        .expect("rooms");
        fs::write(
            root.join("rules.json"),
            r#"{"blocked":[{"from":"beta","to":"alpha","reason":"held for the test"}]}"#,
        )
        .expect("rules");
    }

    /// A well-formed letter lands in `room`'s canonical inbox with no receipt.
    fn write_letter(root: &Path, room: &str, from: &str) -> String {
        let (stamp, sent) = crate::mailbox::local_timestamp().expect("timestamp");
        let id = format!("{stamp}-{:06x}", rand_suffix());
        let envelope = crate::model::Envelope {
            id: id.clone(),
            from: from.to_owned(),
            to: room.to_owned(),
            kind: crate::model::MailKind::Letter,
            subject: "arrived after the plan".to_owned(),
            sent,
            from_participant: None,
            from_lineage: None,
            address_kind: Some("workspace".to_owned()),
            to_host: None,
            display_name: None,
            pfp: None,
            sender_address: None,
            sender_provenance: None,
        };
        let inbox = root.join(room).join("inbox");
        fs::create_dir_all(&inbox).expect("inbox");
        fs::write(
            inbox.join(format!("{id}.mail")),
            crate::mailbox::encode_mail(&envelope, "body\n").expect("encode"),
        )
        .expect("letter");
        id
    }

    fn rand_suffix() -> u32 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0xabc000);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    fn workspace(name: &str) -> participant::Address {
        participant::Address {
            kind: participant::AddressKind::Workspace,
            name: name.to_owned(),
        }
    }

    /// The blocker from review: `apply` used to re-check only activity and the
    /// participant's own inbox, so mail or a subscription that arrived after
    /// the plan could still lose its identity. Every kind of arrival keeps it.
    #[test]
    fn state_that_arrives_after_planning_keeps_the_participant() {
        type Arrive = fn(&Context, &str);
        // The middle flag: whether the arrival also keeps a candidate that holds
        // state. A tier-2 record is kept only for mail that is its own, and
        // workspace mail is pending for its active members, not for an idle
        // one; an empty record is kept more cautiously, for any mail waiting in
        // its workspace.
        let scenarios: [(&str, bool, Arrive); 7] = [
            // Planned as empty, then it is not: the record would now be
            // archived, not deleted, and a delete would lose what arrived.
            ("state written into its directory", false, |context, id| {
                fs::write(
                    context
                        .root
                        .join("participants")
                        .join(id)
                        .join("cursors.json"),
                    b"{\"version\":2,\"mail\":{},\"channels\":{}}\n",
                )
                .expect("state");
            }),
            ("a letter sent straight to it", true, |context, id| {
                write_letter(&context.root, &format!("participants/{id}"), "gamma");
            }),
            ("mail pending in its workspace", false, |context, _| {
                let letter = write_letter(&context.root, "alpha", "gamma");
                let summary =
                    routing::pending_summary(context, &workspace("alpha")).expect("pending");
                assert_eq!(summary.pending, vec![letter], "it is pending, not held");
            }),
            ("mail its workspace holds", false, |context, _| {
                let letter = write_letter(&context.root, "alpha", "beta");
                let held = routing::held_ids(context, &workspace("alpha")).expect("held");
                assert_eq!(held, vec![letter], "the rule holds it");
            }),
            ("a doorbell subscription", true, |context, id| {
                let prefs = context.root.join("doorbell/prefs");
                fs::create_dir_all(&prefs).expect("prefs");
                fs::write(prefs.join(format!("{id}.json")), b"{}\n").expect("prefs file");
            }),
            ("a fresh watch heartbeat", true, |context, id| {
                let record = participant::load(context, id)
                    .expect("load")
                    .expect("record");
                crate::presence::touch_participant_heartbeat(&record, 10_000);
                assert!(crate::presence::participant_heartbeat_stamp(&record).is_some());
            }),
            ("a receipt that names it", true, |context, id| {
                let letter = write_letter(&context.root, "alpha", "gamma");
                let bytes = fs::read(context.root.join(format!("alpha/inbox/{letter}.mail")))
                    .expect("the letter");
                let receipt = routing::Receipt {
                    version: 1,
                    message: letter.clone(),
                    digest: format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes)),
                    address: workspace("alpha"),
                    recipients: vec![id.to_owned()],
                    excluded: Vec::new(),
                    routed_at: "2026-09-28 10:00:00 +0000".to_owned(),
                    routed_by: "post".to_owned(),
                };
                let directory = context.root.join("alpha/routing");
                fs::create_dir_all(&directory).expect("routing dir");
                fs::write(
                    directory.join(format!("{letter}.json")),
                    serde_json::to_vec(&receipt).expect("receipt"),
                )
                .expect("receipt file");
            }),
        ];
        // Both tiers: a record that holds nothing (deleted with a tombstone)
        // and one that holds state (moved to the archive).
        for (with_state, tier) in [(false, Tier::Delete), (true, Tier::Archive)] {
            for (name, keeps_state_holders, arrive) in scenarios {
                if with_state && !keeps_state_holders {
                    continue;
                }
                let name = format!("{name} ({tier:?})");
                let root = test_root("gc-arrival");
                let context = context_at(&root);
                write_rooms_and_rules(&root);
                // The workspace has an active member, so a letter to it has
                // someone to be pending or held for.
                let now = SystemTime::now();
                seed_in(
                    &root,
                    "active-member",
                    &days_ago(0, now),
                    false,
                    Some("alpha"),
                );
                let (id, _) = seed_in(
                    &root,
                    "idle-member",
                    &days_ago(40, now),
                    with_state,
                    Some("alpha"),
                );
                let plan = plan(&context, now).expect("plan");
                assert_eq!(plan.ids(tier), vec![id.clone()], "{name}: planned");

                arrive(&context, &id);

                let applied = apply_plan(&context, &plan, now, &mut |_| Ok(())).expect("apply");
                assert!(
                    applied.deleted.is_empty() && applied.archived.is_empty(),
                    "{name}: the participant was collected"
                );
                assert_eq!(applied.skipped, 1, "{name}: reported as changed");
                assert!(
                    root.join("participants")
                        .join(&id)
                        .join("participant.json")
                        .is_file(),
                    "{name}: the record is gone"
                );
                assert!(
                    !gc::tombstones_path(&context).exists(),
                    "{name}: a tombstone was written"
                );
                assert!(
                    !gc::archived_dir(&context, &id).exists(),
                    "{name}: it was archived"
                );
                trash_test_root(&root);
            }
        }
    }

    /// A cached workspace answer is reused only while the inbox looks exactly as
    /// it did, and never on a stamp that could not be taken.
    #[test]
    fn a_cached_answer_needs_a_stamp_that_still_matches() {
        let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let later = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        assert!(
            cache_still_valid(false, None, None),
            "a plan trusts its snapshot"
        );
        assert!(cache_still_valid(true, Some((then, 2)), Some((then, 2))));
        assert!(
            !cache_still_valid(true, Some((then, 2)), Some((later, 2))),
            "touched"
        );
        assert!(
            !cache_still_valid(true, Some((then, 2)), Some((then, 3))),
            "a letter added"
        );
        assert!(
            !cache_still_valid(true, Some((then, 2)), None),
            "now unreadable"
        );
        assert!(
            !cache_still_valid(true, None, None),
            "never stamped, never proof"
        );
    }

    /// The re-check is per candidate, immediately before its own delete: a
    /// letter that lands while an earlier candidate in the same batch is being
    /// collected keeps the later one.
    #[test]
    fn a_candidate_is_checked_again_just_before_its_own_delete() {
        let root = test_root("gc-per-candidate");
        let context = context_at(&root);
        write_rooms_and_rules(&root);
        fs::create_dir_all(root.join("alpha/inbox")).expect("inbox exists, empty");
        let now = SystemTime::now();
        seed_in(
            &root,
            "first-idle",
            &days_ago(40, now),
            false,
            Some("alpha"),
        );
        seed_in(
            &root,
            "second-idle",
            &days_ago(40, now),
            false,
            Some("alpha"),
        );
        let plan = plan(&context, now).expect("plan");
        assert_eq!(plan.actions.len(), 2, "both planned");
        let second = plan.actions[1].id().to_owned();
        let first = plan.actions[0].id().to_owned();

        let mut checks = 0;
        let applied = apply_plan(&context, &plan, now, &mut |step| {
            if step == Step::BeforeCheck {
                checks += 1;
                if checks == 2 {
                    write_letter(&root, "alpha", "gamma");
                }
            }
            Ok(())
        })
        .expect("apply");
        assert_eq!(applied.deleted, vec![first], "the first was collected");
        assert_eq!(applied.skipped, 1, "the second is reported as changed");
        assert!(
            root.join("participants")
                .join(&second)
                .join("participant.json")
                .is_file(),
            "the letter that landed before the second check keeps it"
        );
        trash_test_root(&root);
    }

    /// A collected record comes back under its own id: a tier-1 record from
    /// its tombstone, a tier-2 record from the archive with its state.
    #[test]
    fn a_collected_record_is_revived_under_the_same_id() {
        let root = test_root("gc-revive");
        let context = context_at(&root);
        let now = SystemTime::now();
        let (bare, digest) = seed_in(
            &root,
            "revive-bare",
            &days_ago(40, now),
            false,
            Some("alpha"),
        );
        let (stateful, _) = seed_in(&root, "revive-stateful", &days_ago(40, now), true, None);
        // What the record says about itself survives its tombstone.
        let record_file = root
            .join("participants")
            .join(&bare)
            .join("participant.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&record_file).expect("record")).expect("json");
        record["lease_hours"] = serde_json::json!(12);
        record["display_name"] = serde_json::json!("Ember");
        record["workspace_path"] = serde_json::json!("/projects/alpha");
        fs::write(
            &record_file,
            serde_json::to_vec_pretty(&record).expect("json"),
        )
        .expect("edit");
        let plan = plan(&context, now).expect("plan");
        let applied = apply_plan(&context, &plan, now, &mut |_| Ok(())).expect("apply");
        assert_eq!(applied.deleted, vec![bare.clone()]);
        assert_eq!(applied.archived, vec![stateful.clone()]);

        let _lock = participant::lock(&context).expect("lock");
        let back = participant::revive_locked(&context, &bare)
            .expect("revive")
            .expect("a tombstoned id comes back");
        assert_eq!(back.id, bare);
        assert_eq!(back.conversation_key_digest, digest);
        assert_eq!(back.workspace.as_deref(), Some("alpha"));
        assert_eq!(back.lease_hours, 12);
        assert_eq!(back.display_name.as_deref(), Some("Ember"));
        assert_eq!(
            back.workspace_path.as_deref(),
            Some(std::path::Path::new("/projects/alpha"))
        );
        assert!(matches!(
            participant::resolve_key(&context, "claude", &digest).expect("resolve"),
            participant::KeyResolution::Live(live) if live.id == bare
        ));

        let restored = participant::revive_locked(&context, &stateful)
            .expect("revive")
            .expect("an archived id comes back");
        assert_eq!(restored.id, stateful);
        assert!(
            restored.dir.join("cursors.json").is_file(),
            "state came back"
        );
        assert!(!gc::archived_dir(&context, &stateful).exists());

        assert!(
            participant::revive_locked(&context, "claude-ffffffff")
                .expect("revive")
                .is_none(),
            "an id that never existed stays missing"
        );
        // An archived record that cannot be read is an error, not a guess.
        let broken = gc::archived_dir(&context, "claude-badbad00");
        fs::create_dir_all(&broken).expect("archive dir");
        fs::write(broken.join("participant.json"), b"not json").expect("garbage record");
        assert!(participant::revive_locked(&context, "claude-badbad00").is_err());
        assert!(
            broken.join("participant.json").is_file(),
            "an unreadable archive stays where it was found"
        );
        assert!(
            !context.root.join("participants/claude-badbad00").exists(),
            "and is not left half-restored in the registry"
        );
        drop(_lock);
        trash_test_root(&root);
    }
}
