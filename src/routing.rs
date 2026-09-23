use crate::error::{AppError, AppResult, ErrorCode};
use crate::lineage;
use crate::mailbox::{atomic_replace, parse_mail, Context};
use crate::participant::{self, Address, AddressKind, Participant};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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
    let _lock = participant::lock(context)?;
    let mut report = RouteReport::default();
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
        match route_message_locked(context, address, id) {
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
        match filtered_recipients(context, address, &parsed.envelope) {
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
    let candidates = store_addresses(context);
    let mut received = Vec::new();
    for address in candidates {
        let directory = routing_dir(context, &address);
        let mut named = false;
        if let Ok(entries) = fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                match receipt(context, &address, id) {
                    Ok(Some(receipt))
                        if receipt.recipients.contains(&participant.id)
                            || canonical_sender_is(context, &address, id, &participant.id) =>
                    {
                        named = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(error) if error.code == ErrorCode::ConfigInvalid => warn_once(
                        path,
                        format!("corrupt routing receipt skipped: {}", error.message),
                    ),
                    Err(error) => return Err(error),
                }
            }
        }
        if !named {
            for path in message_files(&inbox_path(context, &address))? {
                if parse_mail(&path).is_ok_and(|mail| {
                    super::eligibility::envelope_is_own(context, participant, &mail.envelope)
                }) {
                    named = true;
                    break;
                }
            }
        }
        if named {
            received.push(address);
        }
    }
    received.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then(left.name.cmp(&right.name))
    });
    received.dedup_by(|left, right| left == right);
    Ok(received)
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

fn canonical_sender_is(context: &Context, address: &Address, id: &str, participant: &str) -> bool {
    let path = inbox_path(context, address).join(format!("{id}.mail"));
    parse_mail(&path).is_ok_and(|mail| {
        crate::output::mail_authored_locally_by(context, participant, &mail.envelope)
    })
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
    let mut ids = Vec::new();
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
        let recipients = match filtered_recipients(context, address, &parsed.envelope) {
            Ok((recipients, _)) => recipients,
            Err(error) if error.code == ErrorCode::BlockedRoute => continue,
            Err(error) => return Err(error),
        };
        if !recipients.iter().any(|id| id == &participant.id) {
            continue;
        }
        if address.kind != AddressKind::Participant
            && super::eligibility::envelope_is_own(context, participant, &parsed.envelope)
        {
            continue;
        }
        ids.push(id.to_owned());
    }
    Ok(ids)
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
        let recipients = candidate_recipients(context, address, &parsed.envelope)?;
        if !recipients.contains(&participant.id) {
            continue;
        }
        match filtered_recipients(context, address, &parsed.envelope) {
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
    if let Some(existing) = receipt(context, address, id)? {
        return Ok(Some(existing));
    }
    let message_path = inbox_path(context, address).join(format!("{id}.mail"));
    let bytes = fs::read(&message_path)
        .map_err(|error| AppError::io("read canonical mail for routing", &message_path, error))?;
    // Parsing pins filename/envelope identity before a receipt can bless the
    // file. The digest below covers the exact immutable bytes.
    let parsed = parse_mail(&message_path)?;
    let (recipients, excluded) = filtered_recipients(context, address, &parsed.envelope)?;
    if recipients.is_empty() {
        return Ok(None);
    }
    let (_, routed_at) = crate::mailbox::local_timestamp()?;
    let routed_by = participant::resolve(context)?
        .participant()
        .map(|actor| actor.id.clone())
        .unwrap_or_else(|| "post".to_owned());
    let receipt = Receipt {
        version: RECEIPT_VERSION,
        message: id.to_owned(),
        digest: hex_sha256(&bytes),
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
    Ok(Some(receipt))
}

fn filtered_recipients(
    context: &Context,
    address: &Address,
    envelope: &crate::model::Envelope,
) -> AppResult<(Vec<String>, Vec<ExcludedRecipient>)> {
    let mut recipients = candidate_recipients(context, address, envelope)?;
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

fn candidate_recipients(
    context: &Context,
    address: &Address,
    envelope: &crate::model::Envelope,
) -> AppResult<Vec<String>> {
    let mut recipients = resolved_recipients(context, address)?;
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
    Ok(recipients)
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

fn message_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
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
