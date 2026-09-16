use crate::error::{AppError, AppResult, ErrorCode};
use crate::lineage;
use crate::mailbox::{atomic_replace, parse_mail, Context};
use crate::participant::{self, Address, AddressKind, Participant};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

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
}

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
        if receipt(context, address, id)?.is_some() {
            continue;
        }
        match route_message_locked(context, address, id) {
            Ok(Some(receipt)) => report.routed.push((id.to_owned(), receipt.recipients)),
            Ok(None) => report.pending += 1,
            Err(error) if error.code == ErrorCode::ConfigInvalid => {
                eprintln!(
                    "post: warning: left malformed mail {:?} pending: {:?}",
                    path.display().to_string(),
                    error.message
                );
                report.pending += 1;
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
    let mut pending = 0;
    for path in message_files(&inbox_path(context, address))? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if receipt(context, address, id)?.is_none() {
            pending += 1;
        }
    }
    Ok(pending)
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

fn provisional_pending(
    context: &Context,
    participant: &Participant,
    address: &Address,
    warn: bool,
) -> AppResult<Vec<String>> {
    let current = resolved_recipients(context, address)?;
    if !current.iter().any(|id| id == &participant.id) {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for path in message_files(&inbox_path(context, address))? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if receipt(context, address, id)?.is_some() {
            continue;
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
        if parsed.envelope.from_participant.as_deref() == Some(participant.id.as_str()) {
            continue;
        }
        ids.push(id.to_owned());
    }
    Ok(ids)
}

fn route_message_locked(
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
    let mut recipients = resolved_recipients(context, address)?;
    if matches!(address.kind, AddressKind::Workspace | AddressKind::Lineage) {
        if let Some(sender) = parsed.envelope.from_participant.as_ref() {
            recipients.retain(|recipient| recipient != sender);
        }
    }
    let excluded = if address.kind == AddressKind::Lineage {
        exclude_blocked_lineage_recipients(context, &parsed.envelope.from, &mut recipients)?
    } else {
        Vec::new()
    };
    if recipients.is_empty() {
        return Ok(None);
    }
    ensure_resolved_routes_allowed(context, &parsed.envelope.from, address)?;
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
}
