use crate::channel::{channel_state_path, parse_channel_message, ChannelPaths, CHANNELS_DIR};
use crate::channel_state;
use crate::cli::DoctorArgs;
use crate::command_result::CommandResult;
use crate::commands::schema::doctor_exit_codes;
use crate::cursor_state::{CURSORS_FILE, CURSORS_LOCK_FILE};
use crate::error::{AppError, AppResult};
use crate::mailbox::{
    parse_mail, validate_component, validate_new_room_name, Context, DEFAULT_ROOMS_JSON,
    DEFAULT_RULES_JSON,
};
use crate::model::{RoomMap, RulesConfig};
use crate::output::{DoctorCheck, DoctorOutput, DoctorSeverity};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

pub(super) fn run(context: &Context, args: DoctorArgs, pretty: bool) -> AppResult<CommandResult> {
    let mut fixed = Vec::new();
    if args.fix {
        if let Err(error) = apply_fixes(context, &mut fixed) {
            let checks = vec![DoctorCheck {
                id: "fix.failed".to_owned(),
                severity: DoctorSeverity::Error,
                path: context.root.display().to_string(),
                message: error.message,
                fixable: false,
                suggested_fix: error.suggested_fix,
            }];
            let output = report(context, checks, fixed);
            return finish(context, output, args.brief, pretty, 3);
        }
    }
    let checks = detect(context);
    let output = report(context, checks, fixed);
    let exit_code = if output.count == 0 { 0 } else { 1 };
    finish(context, output, args.brief, pretty, exit_code)
}

/// Emit the doctor result: the full JSON report by default, or a single
/// summary line under --brief. Exit codes are identical either way.
fn finish(
    context: &Context,
    output: DoctorOutput,
    brief: bool,
    pretty: bool,
    exit_code: i32,
) -> AppResult<CommandResult> {
    let mut result = if brief {
        CommandResult::success(brief_line(&output))
    } else {
        let mut value = serde_json::to_value(&output).map_err(|error| {
            AppError::invalid_argument(format!("serialize doctor report: {error}"))
        })?;
        let resolved = crate::participant::resolve(context)?;
        let (participant, pending) = match &resolved {
            crate::participant::Resolved::Bound {
                participant,
                provenance,
            } => {
                let mut pending = BTreeMap::new();
                for address in super::inbox::visible_addresses(participant) {
                    pending.insert(
                        super::inbox::address_label(&address),
                        crate::cursor_state::routing::provisional_pending_for(
                            context,
                            participant,
                            &address,
                        )?
                        .len(),
                    );
                }
                (
                    serde_json::json!({
                        "status": "bound",
                        "id": participant.id,
                        "provenance": provenance.as_str(),
                        "workspace": participant.workspace,
                        "lineage": participant.lineage,
                    }),
                    pending,
                )
            }
            crate::participant::Resolved::Unbound => {
                let mut pending = BTreeMap::new();
                for room in context.load_rooms()?.into_keys() {
                    let address = crate::participant::Address {
                        kind: crate::participant::AddressKind::Workspace,
                        name: room,
                    };
                    pending.insert(
                        super::inbox::address_label(&address),
                        crate::cursor_state::routing::pending_count(context, &address)?,
                    );
                }
                (
                    serde_json::json!({
                        "status": "unbound",
                        "fix": "run: post participant bind"
                    }),
                    pending,
                )
            }
        };
        let object = value.as_object_mut().expect("doctor output is an object");
        object.insert("participant".to_owned(), participant);
        object.insert(
            "pending".to_owned(),
            serde_json::to_value(pending).expect("pending map"),
        );
        CommandResult::json(&value, pretty)?
    };
    result.exit_code = exit_code;
    Ok(result)
}

/// The one-line --brief summary. Healthy mailboxes name how many checks ran;
/// anything else points back at the full report for the detail.
fn brief_line(output: &DoctorOutput) -> String {
    if output.count == 0 {
        format!("post doctor: ok ({} checks)\n", output.checks.len())
    } else {
        format!(
            "post doctor: {} findings (run post doctor for detail)\n",
            output.count
        )
    }
}

fn report(context: &Context, checks: Vec<DoctorCheck>, fixed: Vec<String>) -> DoctorOutput {
    // Info checks (owner state etc.) are surface, not findings: they never
    // flip ok/status/count, so a healthy configured mailbox still exits 0.
    let findings = checks
        .iter()
        .filter(|check| check.severity != DoctorSeverity::Info)
        .count();
    let status = if findings == 0 {
        "healthy"
    } else if checks
        .iter()
        .any(|check| check.severity == DoctorSeverity::Error)
    {
        "broken"
    } else {
        "degraded"
    };
    DoctorOutput {
        ok: findings == 0,
        status: status.to_owned(),
        root: context.root.display().to_string(),
        count: findings,
        checks,
        fixed,
        exit_codes: doctor_exit_codes(),
    }
}

fn detect(context: &Context) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    if !context.root.is_dir() {
        checks.push(check(
            "root.missing",
            DoctorSeverity::Error,
            &context.root,
            "mailbox root directory does not exist",
            true,
            "Run `post doctor --fix` to create the mailbox root and defaults.",
        ));
        return checks;
    }

    let rooms_path = context.root.join("rooms.json");
    let rules_path = context.root.join("rules.json");
    let rooms = detect_rooms(context, &rooms_path, &mut checks);
    detect_rules(&rules_path, rooms.as_ref(), &mut checks);
    detect_dir(&context.root.join("archive"), "dir.archive", &mut checks);
    detect_participant_lifecycle(context, &mut checks);

    // owner.json is the trust anchor: a broken one makes every
    // badge-computing chat read fail closed (A0a Decision 3), so doctor
    // flags it rather than silently degrading. Absence is not a finding —
    // feature-absent and the legacy 'trey' fallback are legitimate states.
    let owner_json_path = context.owner_json_path();
    let owner_resolution = rooms
        .as_ref()
        .map(|rooms| crate::mailbox::load_owner_with_rooms(context, rooms));
    // Registry-independent fallback: a broken rooms.json must not hide a
    // malformed trust anchor.
    let owner_error: Option<String> = match rooms.as_ref() {
        Some(rooms) => crate::mailbox::load_owner_with_rooms(context, rooms)
            .err()
            .map(|error| error.message.to_string()),
        None => crate::mailbox::check_owner_parses(context)
            .err()
            .map(|error| error.message.to_string()),
    };
    if let Some(error) = owner_error {
        checks.push(check(
            "owner.invalid",
            DoctorSeverity::Error,
            &owner_json_path,
            &format!(
                "trust anchor cannot be used ({error}); badge-computing chat reads fail closed until it is fixed"
            ),
            false,
            "Fix or delete owner.json by hand; `post owner init` recreates it create-only.",
        ));
    } else {
        detect_owner_surface(
            context,
            &owner_json_path,
            owner_resolution.as_ref(),
            &mut checks,
        );
    }

    // profiles.json is optional and presentation-only: delivery never
    // depends on it (stamping silently degrades to no profile), so a
    // malformed registry is a warning that profiles stopped rendering, not
    // a delivery fault. Entries that parse but no longer validate are also
    // surfaced — stamp_for drops them silently, so doctor is where a room
    // learns its stored name/pfp went inert.
    let profiles_path = context.root.join(crate::profile::PROFILES_FILE);
    if profiles_path.exists() {
        match crate::profile::load_profiles(context) {
            Err(error) => checks.push(check(
                "profiles.invalid",
                DoctorSeverity::Warning,
                &profiles_path,
                &format!(
                    "profile registry cannot be loaded ({}); sends still deliver but stamp no profiles until it parses",
                    error.message
                ),
                false,
                "Fix or delete profiles.json by hand; `post profile set` will recreate it.",
            )),
            Ok(profiles) => {
                if let Some(rooms) = rooms.as_ref() {
                    // Stored profiles are validated against the same owner
                    // reservation profile set enforces; a broken anchor
                    // degrades the check to no-owner (owner.invalid above
                    // already reported it).
                    let owner_room = owner_resolution
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .and_then(crate::mailbox::resolved_owner_room);
                    for (room, profile) in &profiles {
                        let mut cleaned = profile.clone();
                        if !rooms.contains_key(room)
                            || crate::profile::drop_invalid_fields(
                                &mut cleaned,
                                room,
                                rooms,
                                owner_room,
                            )
                        {
                            checks.push(check(
                                &format!("profiles.{room}.inert"),
                                DoctorSeverity::Warning,
                                &profiles_path,
                                "stored profile entry no longer validates (or its room is unregistered) and will not stamp or render",
                                false,
                                "Re-run `post profile set` from that room, or remove the entry.",
                            ));
                        }
                    }
                }
            }
        }
    }

    if let Some(rooms) = rooms {
        for (name, path) in rooms {
            match context.expand_room_path(&path) {
                Ok(workspace) if !workspace.is_dir() => checks.push(check(
                    &format!("room.{name}.workspace_missing"),
                    DoctorSeverity::Warning,
                    &workspace,
                    &format!("registered workspace for room '{name}' does not exist"),
                    false,
                    "Create the workspace or correct its path in rooms.json by hand.",
                )),
                Ok(_) => {}
                Err(reason) => checks.push(check(
                    &format!("room.{name}.path_invalid"),
                    DoctorSeverity::Error,
                    &rooms_path,
                    &reason,
                    false,
                    "Correct the room path in rooms.json by hand.",
                )),
            }
            let room_dir = context.root.join(&name);
            detect_dir(
                &room_dir.join("inbox"),
                &format!("room.{name}.inbox_missing"),
                &mut checks,
            );
            detect_dir(
                &room_dir.join("read"),
                &format!("room.{name}.read_missing"),
                &mut checks,
            );
            detect_cursor_state(context, &name, &mut checks);
        }
    }

    scan_mailbox_state(context, &mut checks);
    // channels/ is not a room (no inbox/read) and not archive, so the room
    // and mailbox scans above skip it naturally; its store gets its own pass.
    detect_channels(context, &mut checks);
    checks.sort_by(|left, right| left.id.cmp(&right.id).then(left.path.cmp(&right.path)));
    checks
}

fn detect_participant_lifecycle(context: &Context, checks: &mut Vec<DoctorCheck>) {
    let now = std::time::SystemTime::now();
    let participants = match crate::participant::list(context) {
        Ok(participants) => participants,
        Err(error) => {
            checks.push(check(
                "participants.invalid",
                DoctorSeverity::Error,
                &context.root.join(crate::participant::PARTICIPANTS_DIR),
                &error.message,
                false,
                "Repair the named participant record by hand, then rerun `post doctor`.",
            ));
            return;
        }
    };
    for participant in participants {
        if participant.state(now) != crate::participant::ParticipantState::Stale {
            continue;
        }
        checks.push(check(
            &format!("participant.{}.stale", participant.id),
            DoctorSeverity::Info,
            &participant.dir.join("participant.json"),
            &format!(
                "participant '{}' is stale; mail already frozen to it is not reassigned when its lease expires",
                participant.id
            ),
            false,
            "Use `post participant touch` only from that live session, or `post participant end` when ending it explicitly.",
        ));
    }
}

/// Validate the channel store: each channel's channel.json and members.json
/// parse, members are registered rooms, and messages/ exists and holds only
/// well-formed .msg files. Read-only, like the rest of doctor — nothing is
/// fixed or moved; a channel is never a `--fix` target because its history is
/// immutable.
fn detect_channels(context: &Context, checks: &mut Vec<DoctorCheck>) {
    let channels_root = context.root.join(CHANNELS_DIR);
    let Ok(entries) = fs::read_dir(&channels_root) else {
        return; // no channels dir yet is healthy, not a finding
    };
    let rooms = context.load_rooms().unwrap_or_default();
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(name) = dir.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let name = name.to_owned();
        if !dir.is_dir() {
            // The membership lock is expected machinery, not a stray.
            if name == ".channels.lock" {
                continue;
            }
            checks.push(check(
                "channels.stray_file",
                DoctorSeverity::Warning,
                &dir,
                "channels/ contains a non-directory that is not a channel",
                false,
                "Inspect the file and move it outside channels/ by hand if it does not belong.",
            ));
            continue;
        }
        let paths = match ChannelPaths::new(context, &name) {
            Ok(paths) => paths,
            Err(error) => {
                checks.push(check(
                    &format!("channel.{name}.invalid_name"),
                    DoctorSeverity::Error,
                    &dir,
                    &error.message,
                    false,
                    "Rename the channel directory to a single path-safe name by hand.",
                ));
                continue;
            }
        };
        if let Err(error) = paths.load_info() {
            checks.push(check(
                &format!("channel.{name}.info_invalid"),
                DoctorSeverity::Error,
                &paths.channel_json,
                &error.message,
                false,
                "Restore a valid channel.json or move the channel aside by hand; nothing is deleted.",
            ));
        }
        match paths.load_members() {
            Err(error) => checks.push(check(
                &format!("channel.{name}.members_invalid"),
                DoctorSeverity::Error,
                &paths.members_json,
                &error.message,
                false,
                "Restore a valid members.json by hand; nothing is deleted.",
            )),
            Ok(members) => {
                for member in members.keys() {
                    if !rooms.contains_key(member) {
                        checks.push(check(
                            &format!("channel.{name}.member_unregistered"),
                            DoctorSeverity::Warning,
                            &paths.members_json,
                            &format!("channel member '{member}' is not a registered room"),
                            false,
                            "Register the room in rooms.json or remove it from members.json by hand.",
                        ));
                    }
                }
            }
        }
        if !paths.messages.is_dir() {
            checks.push(check(
                &format!("channel.{name}.messages_missing"),
                DoctorSeverity::Error,
                &paths.messages,
                "channel messages/ directory is missing",
                false,
                "Restore the messages/ directory by hand; nothing is deleted.",
            ));
        } else if let Ok(items) = fs::read_dir(&paths.messages) {
            for item in items.flatten() {
                let message_path = item.path();
                if !message_path.is_file() {
                    continue;
                }
                if message_path.extension().and_then(|value| value.to_str()) != Some("msg") {
                    checks.push(check(
                        "channels.stray_file",
                        DoctorSeverity::Warning,
                        &message_path,
                        "channel messages/ directory contains a non-.msg file",
                        false,
                        "Inspect the file and move it outside the channel by hand if it does not belong.",
                    ));
                } else if let Err(error) = parse_channel_message(&message_path) {
                    checks.push(check(
                        "channels.malformed_message",
                        DoctorSeverity::Error,
                        &message_path,
                        &error.message,
                        false,
                        "Restore a valid .msg envelope/body separator or move the file aside by hand; nothing is deleted.",
                    ));
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorSetShape {
    seen: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorDocumentShape {
    version: u64,
    mail: CursorSetShape,
    channels: BTreeMap<String, CursorSetShape>,
}

/// Check one room's unified cursor state and its lock without opening or
/// repairing either path. Runtime reads intentionally degrade malformed state
/// to an empty snapshot; doctor keeps that degradation visible to operators.
fn detect_cursor_state(context: &Context, room: &str, checks: &mut Vec<DoctorCheck>) {
    let room_dir = context.root.join(room);
    let cursor_path = room_dir.join(CURSORS_FILE);
    let lock_path = room_dir.join(CURSORS_LOCK_FILE);
    let legacy_path = match channel_state_path(context, room) {
        Ok(path) => path,
        Err(_) => return,
    };

    let cursor_exists = match fs::symlink_metadata(&cursor_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    };
    if cursor_exists {
        if let Err(reason) = validate_cursor_file(&cursor_path) {
            checks.push(check(
                &format!("cursor_state.{room}.invalid"),
                DoctorSeverity::Warning,
                &cursor_path,
                &format!("cursors.json cannot be used ({reason}); reads degrade to all unread"),
                false,
                "Inspect or remove cursors.json by hand; reads currently degrade to all unread and `post doctor --fix` never changes cursor state.",
            ));
        }
    }

    let lock_invalid = match fs::symlink_metadata(&lock_path) {
        Ok(metadata) => !trusted_cursor_lock(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => cursor_exists,
        Err(_) => true,
    };
    if lock_invalid {
        let reason = if cursor_exists {
            "cursor state exists without a trusted solitary 0600 lock"
        } else {
            "cursor lock is not a solitary regular 0600 file"
        };
        checks.push(check(
            &format!("cursor_lock.{room}.invalid"),
            DoctorSeverity::Warning,
            &lock_path,
            reason,
            false,
            "Inspect or remove the cursor lock path by hand; `post doctor --fix` never creates, repairs, or deletes cursor state.",
        ));
    }

    // The legacy file remains useful rollback evidence after materialization,
    // but is no longer a live reader state once cursors.json exists. Before
    // materialization it is the read baseline and keeps the old error check.
    let legacy_exists = match fs::symlink_metadata(&legacy_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    };
    if !legacy_exists {
        return;
    }
    let legacy_valid = fs::symlink_metadata(&legacy_path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.nlink() == 1)
        && fs::read(&legacy_path)
            .map(|bytes| channel_state::stored_shape_is_valid(&bytes))
            .unwrap_or(false);
    if legacy_valid {
        if !cursor_exists {
            checks.push(check(
                &format!("cursor_state.{room}.legacy"),
                DoctorSeverity::Info,
                &legacy_path,
                "valid channel-state.json will import on the first consuming cursor write",
                false,
                "Run a consuming read or catchup to materialize cursors.json; the legacy file remains untouched.",
            ));
        }
    } else {
        checks.push(check(
            &format!("channel_state.{room}.invalid"),
            if cursor_exists {
                DoctorSeverity::Warning
            } else {
                DoctorSeverity::Error
            },
            &legacy_path,
            "channel-state.json is neither a v1 {channel: last-read-id} map nor a v2 seen-set document",
            false,
            "Correct or remove the reader's channel-state.json by hand; the channel history is untouched.",
        ));
    }
}

fn validate_cursor_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("path is a symlink".to_owned());
    }
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err("path is not a solitary regular file".to_owned());
    }
    if metadata.permissions().mode() & 0o7777 != 0o600 {
        return Err("path is not mode 0600".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let document: CursorDocumentShape =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if document.version != 1 {
        return Err(format!(
            "unsupported version {}; expected v1",
            document.version
        ));
    }
    validate_seen_ids(&document.mail.seen, "mail", is_canonical_mail_id)?;
    for (channel, set) in document.channels {
        crate::channel::validate_channel_name(&channel)
            .map_err(|_| format!("invalid channel name '{channel}'"))?;
        validate_seen_ids(
            &set.seen,
            "channel",
            crate::channel::is_canonical_channel_message_id,
        )?;
    }
    Ok(())
}

fn validate_seen_ids(ids: &[String], kind: &str, is_valid: fn(&str) -> bool) -> Result<(), String> {
    let mut previous = None;
    for id in ids {
        if !is_valid(id) {
            return Err(format!("invalid {kind} id '{id}'"));
        }
        if previous.is_some_and(|prior: &str| prior >= id.as_str()) {
            return Err(format!("{kind} ids must be sorted and duplicate-free"));
        }
        previous = Some(id.as_str());
    }
    Ok(())
}

fn is_canonical_mail_id(id: &str) -> bool {
    let id = id.as_bytes();
    id.len() == 22
        && id[..8].iter().all(u8::is_ascii_digit)
        && id[8] == b'-'
        && id[9..15].iter().all(u8::is_ascii_digit)
        && id[15] == b'-'
        && id[16..].iter().all(u8::is_ascii_hexdigit)
}

fn trusted_cursor_lock(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.permissions().mode() & 0o7777 == 0o600
}

fn detect_rooms(context: &Context, path: &Path, checks: &mut Vec<DoctorCheck>) -> Option<RoomMap> {
    if !path.is_file() {
        checks.push(check(
            "config.rooms_missing",
            DoctorSeverity::Error,
            path,
            "rooms.json is missing",
            true,
            "Run `post doctor --fix` to create the default rooms.json.",
        ));
        return None;
    }
    match fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RoomMap>(&bytes).ok())
    {
        Some(rooms) => {
            if rooms.is_empty() {
                checks.push(check(
                    "config.rooms_empty",
                    DoctorSeverity::Info,
                    path,
                    "no rooms registered",
                    false,
                    "Register the first room with `post rooms add <name> <path>`.",
                ));
            } else {
                for (name, value) in &rooms {
                    if let Err(reason) = validate_new_room_name(name) {
                        checks.push(check(
                            &format!("config.room_name.{name}"),
                            DoctorSeverity::Error,
                            path,
                            &reason,
                            false,
                            "Replace the invalid key in rooms.json with one path-safe component.",
                        ));
                    }
                    if let Err(reason) = context.expand_room_path(value) {
                        checks.push(check(
                            &format!("config.room_path.{name}"),
                            DoctorSeverity::Error,
                            path,
                            &reason,
                            false,
                            "Replace the invalid room path with an absolute or '~/...' path.",
                        ));
                    }
                }
            }
            Some(rooms)
        }
        _ => {
            checks.push(check(
                "config.rooms_invalid",
                DoctorSeverity::Error,
                path,
                "rooms.json is not a non-empty JSON object of string paths",
                false,
                "Correct rooms.json by hand; `post doctor --fix` never overwrites config content.",
            ));
            None
        }
    }
}

fn detect_rules(path: &Path, rooms: Option<&RoomMap>, checks: &mut Vec<DoctorCheck>) {
    if !path.is_file() {
        checks.push(check(
            "config.rules_missing",
            DoctorSeverity::Error,
            path,
            "rules.json is missing",
            true,
            "Run `post doctor --fix` to create the default rules.json.",
        ));
        return;
    }
    let parsed = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RulesConfig>(&bytes).ok());
    let Some(rules) = parsed else {
        checks.push(check(
            "config.rules_invalid",
            DoctorSeverity::Error,
            path,
            "rules.json does not match {\"blocked\":[{\"from\",\"to\",\"reason\"}]} with strings",
            false,
            "Correct rules.json by hand; `post doctor --fix` never overwrites rule content.",
        ));
        return;
    };
    for (index, rule) in rules.blocked.iter().enumerate() {
        let invalid_from = rule.from != "*" && validate_component(&rule.from).is_err();
        let invalid_to = rule.to != "*" && validate_component(&rule.to).is_err();
        let unknown_to = rule.to != "*" && rooms.is_some_and(|rooms| !rooms.contains_key(&rule.to));
        if rule.from.trim().is_empty()
            || rule.to.trim().is_empty()
            || rule.reason.trim().is_empty()
            || invalid_from
            || invalid_to
            || unknown_to
        {
            checks.push(check(
                &format!("config.rule.{index}"),
                DoctorSeverity::Error,
                path,
                &format!(
                    "blocked[{index}] has empty/unsafe fields or names unknown recipient '{}'",
                    rule.to
                ),
                false,
                "Correct the named blocked rule in rules.json by hand.",
            ));
        }
    }
}

fn detect_dir(path: &Path, id: &str, checks: &mut Vec<DoctorCheck>) {
    if !path.is_dir() {
        checks.push(check(
            id,
            DoctorSeverity::Error,
            path,
            "required mailbox directory is missing",
            true,
            "Run `post doctor --fix` to create missing mailbox directories.",
        ));
    }
}

fn scan_mailbox_state(context: &Context, checks: &mut Vec<DoctorCheck>) {
    let Ok(entries) = fs::read_dir(&context.root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let is_archive = path.file_name().and_then(|name| name.to_str()) == Some("archive");
        let dirs = if is_archive {
            vec![path]
        } else {
            vec![path.join("inbox"), path.join("read")]
        };
        for dir in dirs.into_iter().filter(|dir| dir.is_dir()) {
            let Ok(items) = fs::read_dir(&dir) else {
                continue;
            };
            for item in items.flatten() {
                let mail_path = item.path();
                if !mail_path.is_file() {
                    continue;
                }
                if mail_path.extension().and_then(|value| value.to_str()) != Some("mail") {
                    checks.push(check(
                        "state.stray_file",
                        DoctorSeverity::Warning,
                        &mail_path,
                        "mailbox directory contains a non-.mail file",
                        false,
                        "Inspect the file and move it outside the mailbox by hand if it does not belong.",
                    ));
                } else {
                    match parse_mail(&mail_path) {
                        Err(error) => checks.push(check(
                            "state.malformed_mail",
                            DoctorSeverity::Error,
                            &mail_path,
                            &error.message,
                            false,
                            "Restore a valid envelope/body separator or move the file aside by hand; nothing is deleted.",
                        )),
                        Ok(_) if !is_archive => {
                            check_archive_copy(context, &mail_path, checks);
                            if dir.file_name().and_then(|name| name.to_str()) == Some("inbox") {
                                check_read_duplicate(&mail_path, checks);
                            }
                        }
                        Ok(_) => {}
                    }
                }
            }
        }
    }
}

/// An id present in both inbox/ and read/ is an interrupted or failed
/// consume: `exclusive_move` hard-linked the mail into read/ but the inbox
/// unlink never completed. The read-path error for this state points people
/// at doctor, so doctor must actually see it. Detect-only, like the archive
/// checks — resolution stays a human decision.
fn check_read_duplicate(delivered: &Path, checks: &mut Vec<DoctorCheck>) {
    let (Some(filename), Some(inbox_dir)) = (delivered.file_name(), delivered.parent()) else {
        return;
    };
    let Some(room_dir) = inbox_dir.parent() else {
        return;
    };
    let read_copy = room_dir.join("read").join(filename);
    let Ok(read_bytes) = fs::read(&read_copy) else {
        return; // no read/ copy is the healthy state
    };
    match fs::read(delivered) {
        Ok(inbox_bytes) if inbox_bytes == read_bytes => checks.push(check(
            "state.read_duplicate",
            DoctorSeverity::Warning,
            delivered,
            "mail exists in both inbox/ and read/ with identical content — an interrupted consume left the inbox copy behind",
            false,
            "Remove the inbox copy by hand to complete the interrupted consume; the read/ copy is the surviving record.",
        )),
        Ok(_) => checks.push(check(
            "state.read_duplicate_mismatch",
            DoctorSeverity::Error,
            delivered,
            "mail exists in both inbox/ and read/ with differing content",
            false,
            "Inspect both copies and reconcile them by hand without deleting either one.",
        )),
        Err(_) => {}
    }
}

fn check_archive_copy(context: &Context, delivered: &Path, checks: &mut Vec<DoctorCheck>) {
    let Some(filename) = delivered.file_name() else {
        return;
    };
    let archive = context.root.join("archive").join(filename);
    match fs::read(&archive) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => checks.push(check(
            "state.archive_missing",
            DoctorSeverity::Error,
            &archive,
            &format!(
                "delivered mail '{}' has no archive copy",
                delivered.display()
            ),
            false,
            "Copy the delivered .mail file into archive with an exclusive no-replace write; do not resend it.",
        )),
        Ok(archive_bytes) => match fs::read(delivered) {
            Ok(delivered_bytes) if delivered_bytes != archive_bytes => checks.push(check(
                "state.archive_mismatch",
                DoctorSeverity::Error,
                &archive,
                &format!(
                    "archive content differs from delivered mail '{}'",
                    delivered.display()
                ),
                false,
                "Inspect both immutable copies and reconcile them by hand without deleting either one.",
            )),
            _ => {}
        },
        Err(_) => {}
    }
}

fn apply_fixes(context: &Context, fixed: &mut Vec<String>) -> Result<(), AppError> {
    create_dir(&context.root, fixed)?;
    if context.write_default_if_missing("rooms.json", DEFAULT_ROOMS_JSON)? {
        fixed.push(context.root.join("rooms.json").display().to_string());
    }
    if context.write_default_if_missing("rules.json", DEFAULT_RULES_JSON)? {
        fixed.push(context.root.join("rules.json").display().to_string());
    }
    create_dir(&context.root.join("archive"), fixed)?;
    if let Ok(rooms) = context.load_rooms() {
        for name in rooms.keys() {
            create_dir(&context.root.join(name).join("inbox"), fixed)?;
            create_dir(&context.root.join(name).join("read"), fixed)?;
        }
    }
    Ok(())
}

/// Walk PATH looking for an EXECUTABLE regular file named `name` (POSIX
/// empty components mean the current directory). Symlinks are followed to
/// their target, which must be a regular file carrying at least one execute
/// bit — a mode-0644 file or a link to one is not a usable tool. Metadata
/// only — never executes the binary, because some tools (bare `ssh-keygen`)
/// prompt to CREATE state when run interactively instead of acting as a
/// lookup.
fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")
        .as_deref()
        .and_then(|path| find_on_path_in(name, path))
}

/// The env-free core of `find_on_path`: same semantics, explicit PATH
/// value, so unit tests exercise the probe without mutating the process
/// environment.
fn find_on_path_in(name: &str, path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    std::env::split_paths(path).find_map(|dir| {
        let candidate = dir.join(name);
        usable_tool(&candidate).then_some(candidate)
    })
}

/// A usable tool at `candidate`: following symlinks, a regular file with at
/// least one execute bit. Never executes anything.
fn usable_tool(candidate: &std::path::Path) -> bool {
    fs::metadata(candidate)
        .map(|metadata| {
            metadata.file_type().is_file() && metadata.permissions().mode() & 0o111 != 0
        })
        .unwrap_or(false)
}

fn create_dir(path: &Path, fixed: &mut Vec<String>) -> Result<(), AppError> {
    if path.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(path)
        .map_err(|error| AppError::io("create doctor repair directory", path, error))?;
    fixed.push(path.display().to_string());
    Ok(())
}

/// A0a Decision 6 surface, emitted only when the trust anchor itself is
/// usable (owner.invalid owns the broken case): the resolution state, the
/// existence of the resolved sidecar layout (sigs/, allowed_signers),
/// ssh-keygen presence, and stored-profile imitation collisions. Never
/// prints secrets — room names and paths only, no key material.
fn detect_owner_surface(
    context: &Context,
    owner_json_path: &Path,
    resolution: Option<&AppResult<crate::mailbox::OwnerResolution>>,
    checks: &mut Vec<DoctorCheck>,
) {
    let Some(resolution) = resolution else {
        // Unknowable (rooms.json unusable and the registry-independent parse
        // succeeded or the file is absent): no state line to report.
        return;
    };
    let owner = match resolution {
        Ok(crate::mailbox::OwnerResolution::Configured(owner)) => {
            checks.push(check(
                "owner.state",
                DoctorSeverity::Info,
                owner_json_path,
                &format!(
                    "owner: configured (room '{}'); verification badges follow the resolved owner",
                    owner.room
                ),
                false,
                "Run `post owner show` for the full resolved configuration.",
            ));
            Some(owner)
        }
        Ok(crate::mailbox::OwnerResolution::Legacy(owner)) => {
            checks.push(check(
                "owner.state",
                DoctorSeverity::Info,
                owner_json_path,
                &format!(
                    "owner: legacy fallback ({}) — no owner.json; consider `post owner init`",
                    owner.room
                ),
                false,
                "Run `post owner init --room <name>` to declare a configured owner.",
            ));
            Some(owner)
        }
        Ok(crate::mailbox::OwnerResolution::None) => {
            checks.push(check(
                "owner.state",
                DoctorSeverity::Info,
                owner_json_path,
                "owner: none configured — verification badges are disabled",
                false,
                "Run `post owner init --room <name>` to declare a signed owner.",
            ));
            None
        }
        Err(_) => None, // owner.invalid already carries this case
    };
    let Some(owner) = owner else {
        return;
    };
    let sigs = owner.sidecar_dir.join("sigs");
    if !sigs.is_dir() {
        checks.push(check(
            "owner.sig_dir_missing",
            DoctorSeverity::Warning,
            &sigs,
            "signed owner's sigs/ directory does not exist; tagged owner messages render Failed until it does",
            false,
            "Run `post owner init` (it creates sigs/) or let porch's onboarding build the sidecar layout.",
        ));
    }
    if !owner.allowed_signers.is_file() {
        checks.push(check(
            "owner.allowed_signers_missing",
            DoctorSeverity::Warning,
            &owner.allowed_signers,
            "owner allowed_signers file does not exist; signature verification cannot run",
            false,
            "porch's onboarding authors the signer line; until then tagged owner messages render Failed.",
        ));
    }
    // Presence is a PATH/metadata probe only, NEVER an execution: bare
    // `ssh-keygen` on an interactive terminal prompts to CREATE a key
    // instead of acting as a lookup, so executing it here could hang the
    // doctor or solicit filesystem mutation.
    if find_on_path("ssh-keygen").is_none() {
        checks.push(check(
            "owner.keygen_missing",
            DoctorSeverity::Warning,
            owner_json_path,
            "ssh-keygen is not on PATH; signature verification cannot run",
            false,
            "Install OpenSSH or ensure ssh-keygen is reachable on PATH.",
        ));
    }
    // A0a Decision 4: existing profiles that collide with the configured
    // owner are flagged, never retroactively rejected. Mirrors the
    // skeleton predicate profile set enforces.
    if let Ok(profiles) = crate::profile::load_profiles(context) {
        let owner_skel = crate::profile::skeleton(&owner.room);
        let profiles_path = context.root.join(crate::profile::PROFILES_FILE);
        for (room, profile) in &profiles {
            let collides = profile
                .name
                .as_deref()
                .map(|name| {
                    !name.trim().is_empty() && crate::profile::skeleton(name.trim()) == owner_skel
                })
                .unwrap_or(false);
            if collides {
                checks.push(check(
                    &format!("owner.imitation_collision.{room}"),
                    DoctorSeverity::Warning,
                    &profiles_path,
                    &format!(
                        "stored display name for room '{room}' imitates the signed owner '{owner_room}' and will never stamp as-is",
                        owner_room = owner.room
                    ),
                    false,
                    "Re-run `post profile set --name '<other name>'` from that room.",
                ));
            }
        }
    }
}

fn check(
    id: &str,
    severity: DoctorSeverity,
    path: &Path,
    message: &str,
    fixable: bool,
    suggested_fix: &str,
) -> DoctorCheck {
    DoctorCheck {
        id: id.to_owned(),
        severity,
        path: path.display().to_string(),
        message: message.to_owned(),
        fixable,
        suggested_fix: suggested_fix.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::find_on_path_in;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn path_var(parts: &[&Path]) -> String {
        // `find_on_path_in` splits with std::env::split_paths; two absolute
        // parts joined by ':' behave identically to the real PATH grammar.
        parts
            .iter()
            .map(|part| part.display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    }

    /// A0b r3 item 2: doctor's keygen probe must require an executable
    /// regular file and FOLLOW symlinks to their target. The probe is
    /// metadata-only, so it can never execute the recording stub.
    #[test]
    fn keygen_probe_requires_executable_and_follows_symlinks() {
        let root = test_root("doctor-keygen");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("bin dir");
        let candidate = bin.join("ssh-keygen");
        fs::write(&candidate, "#!/bin/sh\necho probe\n").expect("plain file");

        // A regular non-executable file must NOT count: doctor reports
        // owner.keygen_missing for it.
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644)).expect("0644");
        assert_eq!(
            find_on_path_in("ssh-keygen", std::ffi::OsStr::new(&path_var(&[&bin]))),
            None,
            "regular non-executable file is not a usable ssh-keygen"
        );

        // The same regular file made executable counts.
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).expect("0755");
        assert_eq!(
            find_on_path_in("ssh-keygen", std::ffi::OsStr::new(&path_var(&[&bin]))),
            Some(candidate.clone()),
            "executable regular file counts as present"
        );

        // A symlink TO an executable target counts as present.
        let link_dir = root.join("linkbin");
        fs::create_dir_all(&link_dir).expect("link dir");
        std::os::unix::fs::symlink(&candidate, link_dir.join("ssh-keygen")).expect("symlink");
        assert_eq!(
            find_on_path_in("ssh-keygen", std::ffi::OsStr::new(&path_var(&[&link_dir]))),
            Some(link_dir.join("ssh-keygen")),
            "symlink to an executable target must count as present"
        );

        // …but a symlink to a NON-executable target must not.
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644))
            .expect("non-exec target");
        assert_eq!(
            find_on_path_in("ssh-keygen", std::ffi::OsStr::new(&path_var(&[&link_dir]))),
            None,
            "symlink to a non-executable target is not a usable ssh-keygen"
        );

        // A dangling symlink never counts.
        fs::remove_file(&candidate).expect("remove target");
        assert_eq!(
            find_on_path_in("ssh-keygen", std::ffi::OsStr::new(&path_var(&[&link_dir]))),
            None,
            "dangling symlink never counts"
        );

        // A directory named ssh-keygen never counts (not a regular file).
        let dir_dir = root.join("dirdir");
        fs::create_dir_all(dir_dir.join("ssh-keygen")).expect("dir named ssh-keygen");
        assert_eq!(
            find_on_path_in("ssh-keygen", std::ffi::OsStr::new(&path_var(&[&dir_dir]))),
            None,
            "a directory is not a usable tool"
        );

        trash_test_root(&root);
    }
}
