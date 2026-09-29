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
    let (deleted, archived, mut kept) = if apply {
        let done = apply_plan(context, &plan, now, &mut |_| Ok(()))?;
        (done.deleted, done.archived, plan.kept.clone())
    } else {
        (
            plan.ids(Tier::Delete),
            plan.ids(Tier::Archive),
            plan.kept.clone(),
        )
    };
    if apply {
        let skipped = plan.actions.len() - deleted.len() - archived.len();
        if skipped > 0 {
            *kept.entry("changed").or_default() += skipped;
        }
    }
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

/// Everything the planner learns about the host at most once per run.
struct Host<'a> {
    context: &'a Context,
    now: SystemTime,
    index: OnceCell<routing::ReceivedIndex>,
    in_flight: OnceCell<HashSet<String>>,
    unrouted: std::cell::RefCell<BTreeMap<String, bool>>,
}

impl<'a> Host<'a> {
    fn new(context: &'a Context, now: SystemTime) -> Self {
        Self {
            context,
            now,
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
        if let Some(known) = self.unrouted.borrow().get(workspace) {
            return *known;
        }
        let address = participant::Address {
            kind: participant::AddressKind::Workspace,
            name: workspace.to_owned(),
        };
        // A store that cannot be read may hold anything: assume it does.
        let unrouted = routing::has_unrouted_mail(self.context, &address).unwrap_or(true);
        self.unrouted
            .borrow_mut()
            .insert(workspace.to_owned(), unrouted);
        unrouted
    }
}

/// Decide what to collect. Reads only; changes nothing.
pub(crate) fn plan(context: &Context, now: SystemTime) -> AppResult<Plan> {
    let host = Host::new(context, now);
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
    /// The tombstone is durable; the directory has not moved.
    AfterTombstone,
    /// The directory is out of the registry; the session index still names it.
    AfterMove,
}

pub(crate) struct Applied {
    pub deleted: Vec<String>,
    pub archived: Vec<String>,
}

/// Carry out a plan, one participant at a time, in small batches under the
/// participants lock. Each participant is re-checked under the lock, so one
/// that woke up since planning is left alone.
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
    };
    for batch in plan.actions.chunks(BATCH) {
        let _lock = participant::lock(context)?;
        gc::sweep_leftovers(context);
        for planned in batch {
            if !still_collectable(context, planned, now)? {
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
        reason: "stateless and idle".to_owned(),
        archived_at: participant::format_rfc3339(now)?,
    })
}

/// The cheap half of the plan's checks, run again under the lock: the record
/// is the one that was planned, still idle, and (tier 1) still holds nothing.
fn still_collectable(context: &Context, planned: &Planned, now: SystemTime) -> AppResult<bool> {
    let Some(current) = participant::load(context, planned.id())? else {
        return Ok(false);
    };
    let before = &planned.participant;
    if current.last_seen != before.last_seen
        || current.ended_at != before.ended_at
        || current.lineage != before.lineage
        || current.is_active(now)
    {
        return Ok(false);
    }
    if planned.tier == Tier::Delete && !holds_only_scaffolding(&current) {
        return Ok(false);
    }
    // A letter sent straight to this participant lands in its own inbox
    // without the registry lock.
    let inbox = current.dir.join("inbox");
    Ok(routing::message_files(&inbox)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::Path;

    fn digest_of(key: &str) -> String {
        format!("{:x}", Sha256::digest(key.as_bytes()))
    }

    fn seed(root: &Path, key: &str, last_seen: &str, with_state: bool) -> (String, String) {
        let digest = digest_of(key);
        let id = format!("claude-{}", &digest[..8]);
        let dir = root.join("participants").join(&id);
        fs::create_dir_all(&dir).expect("participant dir");
        let record = serde_json::json!({
            "version": 1,
            "id": id,
            "harness": "claude",
            "conversation_key_digest": digest,
            "created": "2026-01-01 00:00:00 +0000",
            "last_seen": last_seen,
            "lease_hours": 24,
        });
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

    fn days_ago(days: u32, now: SystemTime) -> String {
        participant::format_rfc3339(now - DAY * days).expect("timestamp")
    }

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
}
