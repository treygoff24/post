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
    pub routed_at: String,
    pub routed_by: String,
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
                    "post: warning: left malformed mail '{}' pending: {}",
                    path.display(),
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

pub(crate) fn touch_participant(context: &Context, participant: &Participant) -> AppResult<()> {
    let lease_hours = match std::env::var("POST_PARTICIPANT_LEASE_HOURS") {
        Ok(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|hours| *hours > 0)
            .ok_or_else(|| {
                AppError::invalid_argument(
                    "POST_PARTICIPANT_LEASE_HOURS must be a positive whole number of hours",
                )
            })?,
        Err(std::env::VarError::NotPresent) => 24,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(AppError::invalid_argument(
                "POST_PARTICIPANT_LEASE_HOURS is not valid UTF-8",
            ));
        }
    };
    let _lock = participant::lock(context)?;
    let path = participant.dir.join("participant.json");
    let bytes = fs::read(&path)
        .map_err(|error| AppError::io("read participant for lifecycle touch", &path, error))?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(&path, format!("invalid participant JSON: {error}")))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| AppError::config(&path, "participant record is not a JSON object"))?;
    let (_, now) = crate::mailbox::local_timestamp()?;
    object.insert("last_seen".to_owned(), serde_json::Value::String(now));
    object.insert("lease_hours".to_owned(), serde_json::json!(lease_hours));
    let mut encoded = serde_json::to_vec_pretty(&value).map_err(|error| {
        AppError::config(
            &path,
            format!("cannot serialize participant lifecycle: {error}"),
        )
    })?;
    encoded.push(b'\n');
    atomic_replace(&path, &encoded)
        .map_err(|error| AppError::io("update participant lifecycle", &path, error))
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
            Err(_) => continue,
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
    if recipients.is_empty() {
        return Ok(None);
    }
    ensure_resolved_routes_allowed(context, &parsed.envelope.from, address, &recipients)?;
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
        AddressKind::Workspace => participant::list(context)?
            .into_iter()
            .filter(|candidate| {
                candidate.workspace.as_deref() == Some(address.name.as_str())
                    && participant_is_active(candidate)
            })
            .map(|candidate| candidate.id)
            .collect(),
        AddressKind::Lineage => match lineage::load(context, &address.name)? {
            Some(lineage) => lineage
                .members(context)?
                .into_keys()
                .filter(|id| {
                    participant::load(context, id)
                        .ok()
                        .flatten()
                        .is_some_and(|candidate| participant_is_active(&candidate))
                })
                .collect(),
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

fn participant_is_active(participant: &Participant) -> bool {
    let path = participant.dir.join("participant.json");
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    if value.get("ended_at").is_some_and(|ended| !ended.is_null()) {
        return false;
    }
    let Some(last_seen) = value.get("last_seen").and_then(serde_json::Value::as_str) else {
        return true;
    };
    let lease_hours = value
        .get("lease_hours")
        .and_then(serde_json::Value::as_u64)
        .filter(|hours| *hours > 0)
        .unwrap_or(24);
    let Some(last_seen) = parse_timestamp(last_seen) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    now.is_some_and(|now| {
        now >= last_seen
            && now.saturating_sub(last_seen)
                <= i64::try_from(lease_hours.saturating_mul(3600)).unwrap_or(i64::MAX)
    })
}

fn parse_timestamp(value: &str) -> Option<i64> {
    let mut fields = value.split_ascii_whitespace();
    let date = fields.next()?;
    let time = fields.next()?;
    let offset = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let mut date = date.split('-');
    let year = date.next()?.parse::<i64>().ok()?;
    let month = date.next()?.parse::<i64>().ok()?;
    let day = date.next()?.parse::<i64>().ok()?;
    if date.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut time = time.split(':');
    let hour = time.next()?.parse::<i64>().ok()?;
    let minute = time.next()?.parse::<i64>().ok()?;
    let second = time.next()?.parse::<i64>().ok()?;
    if time.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let sign = match offset.as_bytes().first().copied()? {
        b'+' => 1_i64,
        b'-' => -1_i64,
        _ => return None,
    };
    if offset.len() != 5 {
        return None;
    }
    let offset_hour = offset.get(1..3)?.parse::<i64>().ok()?;
    let offset_minute = offset.get(3..5)?.parse::<i64>().ok()?;
    if offset_hour > 23 || offset_minute > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    Some(
        days.saturating_mul(86_400)
            .saturating_add(hour * 3600 + minute * 60 + second)
            .saturating_sub(sign * (offset_hour * 3600 + offset_minute * 60)),
    )
}

fn days_from_civil(mut year: i64, month: i64, day: i64) -> Option<i64> {
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    // Reject impossible dates by round-tripping the coarse month bounds most
    // likely to be hand-edited incorrectly. Production timestamps come from
    // Post itself; this parser's job is fail-closed lifecycle classification.
    let month_lengths = [31_i64, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    (day <= month_lengths[usize::try_from(month - 1).ok()?]).then_some(days)
}

fn ensure_resolved_routes_allowed(
    context: &Context,
    sender: &str,
    address: &Address,
    recipients: &[String],
) -> AppResult<()> {
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    if let Some(rule) = rules.blocked.iter().find(|rule| {
        rule.matches_route(sender, &address.name)
            || recipients
                .iter()
                .any(|recipient| rule.matches_route(sender, recipient))
    }) {
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
