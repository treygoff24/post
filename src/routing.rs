use crate::error::{AppError, AppResult, ErrorCode};
use crate::lineage;
use crate::mailbox::{atomic_replace, parse_mail, Context};
use crate::participant::{self, Address, AddressKind, Participant};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const RECEIPT_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub version: u64,
    pub message: String,
    pub digest: String,
    pub address: Address,
    pub recipients: Vec<String>,
    #[serde(default)]
    pub excluded: Vec<ExcludedRecipient>,
    pub routed_at: String,
    pub routed_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExcludedRecipient {
    pub participant: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RouteReport {
    pub routed: Vec<(String, Vec<String>)>,
    pub pending: usize,
    pub held: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PendingSummary {
    pub pending: Vec<String>,
    pub held: Vec<String>,
    pub unreadable: Vec<String>,
}

static WARNED_RECORDS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

pub(crate) fn inbox_path(context: &Context, address: &Address) -> PathBuf {
    match address.kind {
        AddressKind::Workspace => context.root.join(&address.name).join("inbox"),
        AddressKind::Lineage => context
            .root
            .join(lineage::LINEAGES_DIR)
            .join(&address.name)
            .join("inbox"),
        AddressKind::Participant => context
            .root
            .join(participant::PARTICIPANTS_DIR)
            .join(&address.name)
            .join("inbox"),
    }
}

pub(crate) fn routing_dir(context: &Context, address: &Address) -> PathBuf {
    inbox_path(context, address)
        .parent()
        .expect("canonical inbox has a parent")
        .join("routing")
}

pub(crate) fn receipt_path(context: &Context, address: &Address, id: &str) -> PathBuf {
    routing_dir(context, address).join(format!("{id}.json"))
}

pub(crate) fn receipt(
    context: &Context,
    address: &Address,
    id: &str,
) -> AppResult<Option<Receipt>> {
    let path = receipt_path(context, address, id);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("read routing receipt", &path, error)),
    };
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(&path, format!("invalid routing receipt: {error}")))?;
    validate_receipt(&path, address, id, &receipt)?;
    Ok(Some(receipt))
}

pub(crate) fn route_message(
    context: &Context,
    address: &Address,
    id: &str,
) -> AppResult<Option<Receipt>> {
    let _lock = participant::lock(context)?;
    route_message_locked(context, address, id)
}

pub(crate) fn route_pending(context: &Context, address: &Address) -> AppResult<RouteReport> {
    // Everything already routed is the steady state: answer it without taking
    // the host-wide participants lock. Mail that lands after this look is
    // routed by the next pass, exactly as mail that lands just after a pass.
    if !has_unrouted_mail(context, address)? {
        return Ok(RouteReport::default());
    }
    let _lock = participant::lock(context)?;
    let mut report = RouteReport::default();
    let mut pass = RoutePass::default();
    for path in message_files(&inbox_path(context, address))? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        match receipt(context, address, id) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) if error.code == ErrorCode::ConfigInvalid => {
                warn_once(
                    receipt_path(context, address, id),
                    format!(
                        "corrupt routing receipt skipped as unreadable: {}",
                        error.message
                    ),
                );
                continue;
            }
            Err(error) => return Err(error),
        }
        match route_message_in_pass(context, address, id, &mut pass) {
            Ok(Some(receipt)) => report.routed.push((id.to_owned(), receipt.recipients)),
            Ok(None) => report.pending += 1,
            Err(error) if error.code == ErrorCode::BlockedRoute => {
                warn_once(
                    path.clone(),
                    format!("left blocked mail held: {}", error.message),
                );
                report.held.push(id.to_owned());
            }
            Err(error) if error.code == ErrorCode::ConfigInvalid => {
                warn_once(
                    path.clone(),
                    format!("skipped unreadable pending mail: {}", error.message),
                );
            }
            Err(error) => return Err(error),
        }
    }
    Ok(report)
}

pub(crate) fn route_for_participant(
    context: &Context,
    participant: &Participant,
) -> AppResult<RouteReport> {
    let mut report = RouteReport::default();
    if let Some(workspace) = participant.workspace.as_ref() {
        merge_report(
            &mut report,
            route_pending(
                context,
                &Address {
                    kind: AddressKind::Workspace,
                    name: workspace.clone(),
                },
            )?,
        );
    }
    merge_report(
        &mut report,
        route_pending(
            context,
            &Address {
                kind: AddressKind::Participant,
                name: participant.id.clone(),
            },
        )?,
    );
    Ok(report)
}

pub(crate) fn pending_count(context: &Context, address: &Address) -> AppResult<usize> {
    Ok(pending_summary(context, address)?.pending.len())
}

pub(crate) fn pending_summary(context: &Context, address: &Address) -> AppResult<PendingSummary> {
    let mut summary = PendingSummary::default();
    let mut resolved = None;
    for path in message_files(&inbox_path(context, address))? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let receipt = match receipt(context, address, id) {
            Ok(receipt) => receipt,
            Err(error) if error.code == ErrorCode::ConfigInvalid => {
                warn_once(
                    receipt_path(context, address, id),
                    format!("corrupt routing receipt skipped: {}", error.message),
                );
                summary.unreadable.push(id.to_owned());
                continue;
            }
            Err(error) => return Err(error),
        };
        if receipt.is_some() {
            continue;
        }
        let parsed = match parse_mail(&path) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn_once(
                    path.clone(),
                    format!("skipped unreadable pending mail: {}", error.message),
                );
                summary.unreadable.push(id.to_owned());
                continue;
            }
        };
        let candidates = resolved_once(context, address, &mut resolved)?;
        match filtered_among(context, address, &parsed.envelope, candidates) {
            Err(error) if error.code == ErrorCode::BlockedRoute => {
                summary.held.push(id.to_owned());
            }
            Ok((recipients, exclusions)) if recipients.is_empty() && !exclusions.is_empty() => {
                summary.held.push(id.to_owned());
            }
            Ok(_) => summary.pending.push(id.to_owned()),
            Err(error) => return Err(error),
        }
    }
    Ok(summary)
}

pub(crate) fn has_unrouted_mail(context: &Context, address: &Address) -> AppResult<bool> {
    for path in message_files(&inbox_path(context, address))? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        match receipt(context, address, id) {
            Ok(None) => return Ok(true),
            Ok(Some(_)) => {}
            Err(error) if error.code == ErrorCode::ConfigInvalid => {
                warn_once(
                    receipt_path(context, address, id),
                    format!(
                        "corrupt routing receipt skipped as unreadable: {}",
                        error.message
                    ),
                );
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

pub(crate) fn provisional_pending_for(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<Vec<String>> {
    provisional_pending(context, participant, address, true)
}

pub(crate) fn provisional_pending_for_quiet(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<Vec<String>> {
    provisional_pending(context, participant, address, false)
}

/// Every canonical store whose frozen receipt names this participant or whose
/// message was authored by it. Membership and workspace rebinding cannot hide
/// frozen deliveries or sender history.
pub(crate) fn received_addresses(
    context: &Context,
    participant: &Participant,
) -> AppResult<Vec<Address>> {
    ReceivedIndex::read(context).received(participant)
}

/// `received_addresses` for every participant from one read of every store.
///
/// Asking per participant reopened every store for every participant, and
/// every participant is itself a store, so `post who` went quadratic in the
/// host's participant count (post-gxz: 100 s at 4,139 participants).
///
/// A participant's answer is still the one its own walk gives. That walk
/// visits a store's receipts in directory order and stops at the first that
/// names the participant; only a store that never names it has its inbox
/// listed for sender history. So a receipt that cannot be read, or an inbox
/// that cannot be listed, fails the participants whose walk reaches it and
/// no one else, and a corrupt receipt is warned about only by a walk that
/// passes it. The index keeps those failures per store instead of raising
/// them while it reads.
pub(crate) struct ReceivedIndex {
    stores: Vec<StoreSenders>,
    /// Participant id -> stores naming it, by index into `stores`.
    naming: HashMap<String, Vec<usize>>,
    /// Stores whose walk can warn or fail for a participant it does not name.
    irregular: Vec<usize>,
}

struct StoreSenders {
    address: Address,
    /// Participant id -> position of the first receipt naming it, by
    /// recipient or as the canonical message's local sender.
    first_named: HashMap<String, usize>,
    /// Corrupt receipts before any failure: position, path, warning.
    corrupt: Vec<(usize, PathBuf, String)>,
    /// The first receipt that could not be read. Nothing after it is read.
    failed: Option<(usize, AppError)>,
    /// Local senders in the canonical inbox, or why it could not be listed.
    /// Not read when a receipt failed: no walk gets that far.
    senders: Result<HashSet<String>, AppError>,
}

impl StoreSenders {
    fn read(context: &Context, address: Address) -> Self {
        let mut first_named = HashMap::new();
        let mut corrupt = Vec::new();
        let mut failed = None;
        let directory = routing_dir(context, &address);
        if let Ok(entries) = fs::read_dir(&directory) {
            let receipts = entries.flatten().map(|entry| entry.path()).filter(|path| {
                path.extension().and_then(|value| value.to_str()) == Some("json")
                    && path.file_stem().and_then(|value| value.to_str()).is_some()
            });
            for (position, path) in receipts.enumerate() {
                let id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .expect("filtered to UTF-8 stems");
                match receipt(context, &address, id) {
                    Ok(Some(receipt)) => {
                        let sender =
                            parse_mail(&inbox_path(context, &address).join(format!("{id}.mail")))
                                .ok()
                                .and_then(|mail| local_author(context, &mail.envelope));
                        for named in receipt.recipients.into_iter().chain(sender) {
                            first_named.entry(named).or_insert(position);
                        }
                    }
                    Ok(None) => {}
                    Err(error) if error.code == ErrorCode::ConfigInvalid => corrupt.push((
                        position,
                        path.clone(),
                        format!("corrupt routing receipt skipped: {}", error.message),
                    )),
                    Err(error) => {
                        failed = Some((position, error));
                        break;
                    }
                }
            }
        }
        let senders = if failed.is_some() {
            Ok(HashSet::new())
        } else {
            message_files(&inbox_path(context, &address)).map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| parse_mail(path).ok())
                    .filter_map(|mail| local_author(context, &mail.envelope))
                    .collect()
            })
        };
        Self {
            address,
            first_named,
            corrupt,
            failed,
            senders,
        }
    }

    /// A store whose walk is not simply "named or not".
    fn irregular(&self) -> bool {
        !self.corrupt.is_empty() || self.failed.is_some() || self.senders.is_err()
    }

    /// One participant's walk of this store: whether the store names it.
    fn names(&self, id: &str) -> AppResult<bool> {
        let named_at = self.first_named.get(id).copied();
        let failed_at = self.failed.as_ref().map(|(position, _)| *position);
        let stop = match (named_at, failed_at) {
            (Some(named), Some(failed)) => Some(named.min(failed)),
            (named, failed) => named.or(failed),
        };
        for (position, path, warning) in &self.corrupt {
            if stop.is_none_or(|stop| *position < stop) {
                warn_once(path.clone(), warning.clone());
            }
        }
        if named_at.is_some() && named_at == stop {
            return Ok(true);
        }
        if let Some((_, error)) = &self.failed {
            return Err(error.clone());
        }
        match &self.senders {
            Ok(senders) => Ok(senders.contains(id)),
            Err(error) => Err(error.clone()),
        }
    }
}

impl ReceivedIndex {
    pub(crate) fn read(context: &Context) -> Self {
        let stores: Vec<StoreSenders> = store_addresses(context)
            .into_iter()
            .map(|address| StoreSenders::read(context, address))
            .collect();
        let mut naming: HashMap<String, Vec<usize>> = HashMap::new();
        let mut irregular = Vec::new();
        for (index, store) in stores.iter().enumerate() {
            if store.irregular() {
                irregular.push(index);
                continue;
            }
            let senders = store.senders.iter().flatten();
            let named: BTreeSet<&String> = store.first_named.keys().chain(senders).collect();
            for id in named {
                naming.entry(id.clone()).or_default().push(index);
            }
        }
        Self {
            stores,
            naming,
            irregular,
        }
    }

    /// Whether any readable store names this participant: a frozen receipt
    /// lists it as a recipient, or it authored a letter there. A store whose
    /// receipts cannot be read cannot deliver anything, so it does not count.
    pub(crate) fn names(&self, id: &str) -> bool {
        self.naming.contains_key(id)
            || self
                .irregular
                .iter()
                .any(|index| self.stores[*index].first_named.contains_key(id))
    }

    /// Stores in `store_addresses` order, failing where that participant's
    /// own walk would.
    pub(crate) fn received(&self, participant: &Participant) -> AppResult<Vec<Address>> {
        let mut visit = self
            .naming
            .get(&participant.id)
            .cloned()
            .unwrap_or_default();
        visit.extend(&self.irregular);
        visit.sort_unstable();
        visit.dedup();
        let mut received = Vec::new();
        for index in visit {
            let store = &self.stores[index];
            if store.names(&participant.id)? {
                received.push(store.address.clone());
            }
        }
        Ok(received)
    }
}

/// The one local participant for which `eligibility::envelope_is_own` holds,
/// if any.
fn local_author(context: &Context, envelope: &crate::model::Envelope) -> Option<String> {
    envelope
        .from_participant
        .as_deref()
        .filter(|id| crate::output::mail_authored_locally_by(context, id, envelope))
        .map(str::to_owned)
}

pub(crate) fn store_addresses(context: &Context) -> Vec<Address> {
    let mut candidates = context
        .load_rooms()
        .unwrap_or_default()
        .into_keys()
        .map(|name| Address {
            kind: AddressKind::Workspace,
            name,
        })
        .collect::<Vec<_>>();
    collect_nested_addresses(
        &context.root.join(lineage::LINEAGES_DIR),
        AddressKind::Lineage,
        &mut candidates,
    );
    collect_nested_addresses(
        &context.root.join(participant::PARTICIPANTS_DIR),
        AddressKind::Participant,
        &mut candidates,
    );
    candidates.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then(left.name.cmp(&right.name))
    });
    candidates.dedup();
    candidates
}

fn collect_nested_addresses(root: &Path, kind: AddressKind, addresses: &mut Vec<Address>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name != "by-session" && entry.path().is_dir() {
            addresses.push(Address { kind, name });
        }
    }
}

fn provisional_pending(
    context: &Context,
    participant: &Participant,
    address: &Address,
    warn: bool,
) -> AppResult<Vec<String>> {
    Ok(pending_mail(context, address, warn)?
        .into_iter()
        .filter(|mail| mail.is_pending_for(participant, address))
        .map(|mail| mail.id)
        .collect())
}

/// An unrouted message and the recipients it would be routed to now, resolved
/// once so any number of participants' pending counts can be read from it.
pub(crate) struct PendingMail {
    pub id: String,
    recipients: HashSet<String>,
    local_author: Option<String>,
}

impl PendingMail {
    pub(crate) fn is_pending_for(&self, participant: &Participant, address: &Address) -> bool {
        self.recipients.contains(&participant.id)
            && !(address.kind != AddressKind::Participant
                && self.local_author.as_deref() == Some(participant.id.as_str()))
    }
}

/// Every unrouted, readable, unblocked message in `address`'s canonical store.
pub(crate) fn pending_mail(
    context: &Context,
    address: &Address,
    warn: bool,
) -> AppResult<Vec<PendingMail>> {
    let mut pending = Vec::new();
    let mut resolved = None;
    for path in message_files(&inbox_path(context, address))? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        match receipt(context, address, id) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) if error.code == ErrorCode::ConfigInvalid => {
                warn_once(
                    receipt_path(context, address, id),
                    format!("corrupt routing receipt skipped: {}", error.message),
                );
                continue;
            }
            Err(error) => return Err(error),
        }
        let parsed = match parse_mail(&path) {
            Ok(parsed) => parsed,
            Err(error) => {
                if warn {
                    let kind = if error.code == ErrorCode::IoError {
                        "unreadable"
                    } else {
                        "malformed"
                    };
                    eprintln!(
                        "post: warning: skipped {kind} pending mail {:?}: {:?}",
                        path.display().to_string(),
                        error.message
                    );
                }
                continue;
            }
        };
        let candidates = resolved_once(context, address, &mut resolved)?;
        let recipients = match filtered_among(context, address, &parsed.envelope, candidates) {
            Ok((recipients, _)) => recipients,
            Err(error) if error.code == ErrorCode::BlockedRoute => continue,
            Err(error) => return Err(error),
        };
        pending.push(PendingMail {
            id: id.to_owned(),
            recipients: recipients.into_iter().collect(),
            local_author: local_author(context, &parsed.envelope),
        });
    }
    Ok(pending)
}

pub(crate) fn warn_once(path: PathBuf, message: String) {
    let warned = WARNED_RECORDS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut warned = warned.lock().expect("routing warning mutex");
    if warned.insert(path.clone()) {
        eprintln!(
            "post: warning: {:?}: {:?}",
            message,
            path.display().to_string()
        );
    }
}

pub(crate) fn held_for(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<Vec<String>> {
    let mut held = Vec::new();
    let mut resolved = None;
    for path in message_files(&inbox_path(context, address))? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        match receipt(context, address, id) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) if error.code == ErrorCode::ConfigInvalid => continue,
            Err(error) => return Err(error),
        }
        let parsed = match parse_mail(&path) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        let candidates = resolved_once(context, address, &mut resolved)?;
        let recipients = candidates_among(context, address, &parsed.envelope, candidates.clone());
        if !recipients.contains(&participant.id) {
            continue;
        }
        match filtered_among(context, address, &parsed.envelope, candidates) {
            Err(error) if error.code == ErrorCode::BlockedRoute => held.push(id.to_owned()),
            Ok(_) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(held)
}

pub(crate) fn held_ids(context: &Context, address: &Address) -> AppResult<Vec<String>> {
    Ok(pending_summary(context, address)?.held)
}

pub(crate) fn route_message_locked(
    context: &Context,
    address: &Address,
    id: &str,
) -> AppResult<Option<Receipt>> {
    route_message_in_pass(context, address, id, &mut RoutePass::default())
}

/// What one routing pass learns once and reuses for every letter it routes.
/// The caller holds the participants lock for the whole pass, so neither
/// answer can change under it: an address's resolved recipients do not depend
/// on the letter, and resolving them lists every participant on the host; the
/// acting participant is the same for every receipt the pass writes. Doing
/// either per letter made a first `read` cost one full roster scan per
/// unrouted letter (50 letters among 2,000 participants took 27 s).
#[derive(Default)]
struct RoutePass {
    resolved: Option<Vec<String>>,
    routed_by: Option<String>,
}

fn route_message_in_pass(
    context: &Context,
    address: &Address,
    id: &str,
    pass: &mut RoutePass,
) -> AppResult<Option<Receipt>> {
    if let Some(existing) = receipt(context, address, id)? {
        return Ok(Some(existing));
    }
    let message_path = inbox_path(context, address).join(format!("{id}.mail"));
    let bytes = fs::read(&message_path)
        .map_err(|error| AppError::io("read canonical mail for routing", &message_path, error))?;
    // Parsing pins filename/envelope identity before a receipt can bless the
    // file. The digest below covers the exact immutable bytes.
    let parsed = parse_mail(&message_path)?;
    let candidates = resolved_once(context, address, &mut pass.resolved)?;
    let (recipients, excluded) = filtered_among(context, address, &parsed.envelope, candidates)?;
    if recipients.is_empty() {
        return Ok(None);
    }
    let routed_by = routed_by_once(context, &mut pass.routed_by)?;
    publish_receipt(
        context, address, id, &bytes, recipients, excluded, routed_by,
    )
    .map(Some)
}

/// The acting participant's id, or `post` when no participant is acting.
fn routed_by_once(context: &Context, routed_by: &mut Option<String>) -> AppResult<String> {
    if let Some(known) = routed_by {
        return Ok(known.clone());
    }
    // A claim that names no record is not an actor either: the receipt is
    // stamped `post`, and the command that asked for the claim reports it.
    let actor = match participant::resolve(context) {
        Ok(resolved) => resolved
            .participant()
            .map(|actor| actor.id.clone())
            .unwrap_or_else(|| "post".to_owned()),
        Err(error) if error.code == crate::error::ErrorCode::ParticipantMissing => {
            "post".to_owned()
        }
        Err(error) => return Err(error),
    };
    *routed_by = Some(actor.clone());
    Ok(actor)
}

/// Route an imported letter that `post bridge deliver` admitted to
/// participant `address`. Admission already decided the route, so the
/// receipt names exactly that participant and the current rules are not
/// consulted: a rule added after admission cannot strand the letter, just
/// as it cannot strand local mail routed at send time. The caller holds the
/// participants lock.
pub(crate) fn route_admitted_import_locked(
    context: &Context,
    address: &Address,
    id: &str,
) -> AppResult<Receipt> {
    debug_assert_eq!(address.kind, AddressKind::Participant);
    if let Some(existing) = receipt(context, address, id)? {
        return Ok(existing);
    }
    let message_path = inbox_path(context, address).join(format!("{id}.mail"));
    let bytes = fs::read(&message_path)
        .map_err(|error| AppError::io("read canonical mail for routing", &message_path, error))?;
    parse_mail(&message_path)?;
    publish_receipt(
        context,
        address,
        id,
        &bytes,
        vec![address.name.clone()],
        Vec::new(),
        "post".to_owned(),
    )
}

fn publish_receipt(
    context: &Context,
    address: &Address,
    id: &str,
    bytes: &[u8],
    recipients: Vec<String>,
    excluded: Vec<ExcludedRecipient>,
    routed_by: String,
) -> AppResult<Receipt> {
    let (_, routed_at) = crate::mailbox::local_timestamp()?;
    let receipt = Receipt {
        version: RECEIPT_VERSION,
        message: id.to_owned(),
        digest: hex_sha256(bytes),
        address: address.clone(),
        recipients,
        excluded,
        routed_at,
        routed_by,
    };
    let directory = routing_dir(context, address);
    fs::create_dir_all(&directory)
        .map_err(|error| AppError::io("create routing receipt directory", &directory, error))?;
    let path = receipt_path(context, address, id);
    let mut encoded = serde_json::to_vec_pretty(&receipt).map_err(|error| {
        AppError::config(&path, format!("cannot serialize routing receipt: {error}"))
    })?;
    encoded.push(b'\n');
    atomic_replace(&path, &encoded)
        .map_err(|error| AppError::io("publish routing receipt", &path, error))?;
    Ok(receipt)
}

/// An address's resolved recipients do not depend on the message, and
/// resolving them lists every participant on the host. A pass over one store
/// resolves them on first need and reuses them for every message after
/// (post-gxz: once per legacy unrouted message made `post doctor` take 4 s).
fn resolved_once(
    context: &Context,
    address: &Address,
    resolved: &mut Option<Vec<String>>,
) -> AppResult<Vec<String>> {
    if let Some(recipients) = resolved {
        return Ok(recipients.clone());
    }
    let recipients = resolved_recipients(context, address)?;
    *resolved = Some(recipients.clone());
    Ok(recipients)
}

/// The address's recipients for one letter, from its already-resolved
/// candidates.
fn filtered_among(
    context: &Context,
    address: &Address,
    envelope: &crate::model::Envelope,
    resolved: Vec<String>,
) -> AppResult<(Vec<String>, Vec<ExcludedRecipient>)> {
    let mut recipients = candidates_among(context, address, envelope, resolved);
    let excluded = if address.kind == AddressKind::Lineage {
        exclude_blocked_lineage_recipients(context, &envelope.from, &mut recipients)?
    } else {
        Vec::new()
    };
    if !recipients.is_empty() {
        ensure_resolved_routes_allowed(context, &envelope.from, address)?;
    }
    Ok((recipients, excluded))
}

fn candidates_among(
    context: &Context,
    address: &Address,
    envelope: &crate::model::Envelope,
    mut recipients: Vec<String>,
) -> Vec<String> {
    // The sender is not its own recipient, but only a LOCAL sender: a
    // remote-origin sender's id is host-local to its origin and may equal a
    // local participant's, who must still receive the message.
    if matches!(address.kind, AddressKind::Workspace | AddressKind::Lineage) {
        if let Some(sender) = envelope.from_participant.as_ref() {
            if !crate::output::mail_remote_origin(context, envelope) {
                recipients.retain(|recipient| recipient != sender);
            }
        }
    }
    recipients
}

pub(crate) fn resolved_recipients(context: &Context, address: &Address) -> AppResult<Vec<String>> {
    let mut recipients = match address.kind {
        AddressKind::Workspace => participant::list_active(context)?
            .into_iter()
            .filter(|candidate| candidate.workspace.as_deref() == Some(address.name.as_str()))
            .map(|candidate| candidate.id)
            .collect(),
        AddressKind::Lineage => match lineage::load(context, &address.name)? {
            Some(lineage) => {
                let members = lineage.members(context)?;
                participant::list_active(context)?
                    .into_iter()
                    .filter(|candidate| members.contains_key(&candidate.id))
                    .map(|candidate| candidate.id)
                    .collect()
            }
            None => Vec::new(),
        },
        AddressKind::Participant => participant::load(context, &address.name)?
            .map(|candidate| vec![candidate.id])
            .unwrap_or_default(),
    };
    recipients.sort();
    recipients.dedup();
    Ok(recipients)
}

fn ensure_resolved_routes_allowed(
    context: &Context,
    sender: &str,
    address: &Address,
) -> AppResult<()> {
    if address.kind == AddressKind::Lineage {
        return Ok(());
    }
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    let recipient = match address.kind {
        AddressKind::Workspace => Some(address.name.clone()),
        AddressKind::Participant => {
            participant::load(context, &address.name)?.and_then(|participant| participant.workspace)
        }
        AddressKind::Lineage => None,
    };
    let rule = rules.blocked.iter().find(|rule| {
        recipient.as_deref().map_or(
            rule.to == "*" && rule.matches_route(sender, "*"),
            |recipient| rule.matches_route(sender, recipient),
        )
    });
    if let Some(rule) = rule {
        return Err(AppError::new(
            crate::error::ErrorCode::BlockedRoute,
            format!(
                "routing {} -> {} would reach a blocked resolved recipient: {}",
                sender, address.name, rule.reason
            ),
            "Do not route around this block. Ask the human operator to review rules.json.",
        )
        .input(format!("{sender} -> {}", address.name))
        .reason(rule.reason.clone())
        .rule(rule.clone()));
    }
    Ok(())
}

fn exclude_blocked_lineage_recipients(
    context: &Context,
    sender: &str,
    recipients: &mut Vec<String>,
) -> AppResult<Vec<ExcludedRecipient>> {
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    let mut excluded = Vec::new();
    recipients.retain(|id| {
        let workspace = participant::load(context, id)
            .ok()
            .flatten()
            .and_then(|participant| participant.workspace);
        let blocked = rules.blocked.iter().any(|rule| {
            workspace.as_deref().map_or(
                rule.to == "*" && (rule.from == "*" || rule.from == sender),
                |workspace| rule.matches_route(sender, workspace),
            )
        });
        if blocked {
            excluded.push(ExcludedRecipient {
                participant: id.clone(),
                reason: "blocked-route".to_owned(),
            });
        }
        !blocked
    });
    excluded.sort_by(|left, right| left.participant.cmp(&right.participant));
    Ok(excluded)
}

pub(crate) fn message_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AppError::io(
                "list canonical mail directory",
                directory,
                error,
            ))
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read canonical mail entry", directory, error))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("mail") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn validate_receipt(path: &Path, address: &Address, id: &str, receipt: &Receipt) -> AppResult<()> {
    if receipt.version != RECEIPT_VERSION
        || receipt.message != id
        || &receipt.address != address
        || receipt.recipients.windows(2).any(|pair| pair[0] >= pair[1])
        || receipt
            .excluded
            .windows(2)
            .any(|pair| pair[0].participant >= pair[1].participant)
        || receipt.excluded.iter().any(|excluded| {
            excluded.reason != "blocked-route" || receipt.recipients.contains(&excluded.participant)
        })
    {
        return Err(AppError::config(
            path,
            "routing receipt does not match its canonical message/address or has unsorted recipients",
        ));
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn merge_report(target: &mut RouteReport, source: RouteReport) {
    target.routed.extend(source.routed);
    target.pending += source.pending;
    target.held.extend(source.held);
}
