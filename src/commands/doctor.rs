use crate::channel::{
    channel_state_path, parse_channel_message, ChannelPaths, ChannelStateMap, CHANNELS_DIR,
};
use crate::cli::DoctorArgs;
use crate::command_result::CommandResult;
use crate::commands::schema::doctor_exit_codes;
use crate::error::{AppError, AppResult};
use crate::mailbox::{
    parse_mail, validate_component, validate_room_name, Context, DEFAULT_ROOMS_JSON,
    DEFAULT_RULES_JSON,
};
use crate::model::{RoomMap, RulesConfig};
use crate::output::{DoctorCheck, DoctorOutput, DoctorSeverity};
use std::fs;
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
            let mut result = CommandResult::json(&output, pretty)?;
            result.exit_code = 3;
            return Ok(result);
        }
    }
    let checks = detect(context);
    let output = report(context, checks, fixed);
    let exit_code = if output.count == 0 { 0 } else { 1 };
    let mut result = CommandResult::json(&output, pretty)?;
    result.exit_code = exit_code;
    Ok(result)
}

fn report(context: &Context, checks: Vec<DoctorCheck>, fixed: Vec<String>) -> DoctorOutput {
    let status = if checks.is_empty() {
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
        ok: checks.is_empty(),
        status: status.to_owned(),
        root: context.root.display().to_string(),
        count: checks.len(),
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
                    for (room, profile) in &profiles {
                        let mut cleaned = profile.clone();
                        if !rooms.contains_key(room)
                            || crate::profile::drop_invalid_fields(&mut cleaned, room, rooms)
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
        }
    }

    scan_mailbox_state(context, &mut checks);
    // channels/ is not a room (no inbox/read) and not archive, so the room
    // and mailbox scans above skip it naturally; its store gets its own pass.
    detect_channels(context, &mut checks);
    checks.sort_by(|left, right| left.id.cmp(&right.id).then(left.path.cmp(&right.path)));
    checks
}

/// Validate the channel store: each channel's channel.json and members.json
/// parse, members are registered rooms, messages/ exists and holds only
/// well-formed .msg files, and each reader's channel-state.json (if any) is a
/// valid cursor map. Read-only, like the rest of doctor — nothing is fixed or
/// moved; a channel is never a `--fix` target because its history is immutable.
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
    // Reader cursors live in each room's own tree; a corrupt one can only
    // hurt that room, but a bad JSON blob silently breaks its reads, so flag it.
    for name in rooms.keys() {
        let Ok(state_path) = channel_state_path(context, name) else {
            continue;
        };
        if !state_path.is_file() {
            continue;
        }
        let parsed = fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ChannelStateMap>(&bytes).ok());
        if parsed.is_none() {
            checks.push(check(
                &format!("channel_state.{name}.invalid"),
                DoctorSeverity::Error,
                &state_path,
                "channel-state.json is not a valid {channel: last-read-id} map",
                false,
                "Correct or remove the reader's channel-state.json by hand; the channel history is untouched.",
            ));
        }
    }
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
        Some(rooms) if !rooms.is_empty() => {
            for (name, value) in &rooms {
                if let Err(reason) = validate_room_name(name) {
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
                            check_archive_copy(context, &mail_path, checks)
                        }
                        Ok(_) => {}
                    }
                }
            }
        }
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

fn create_dir(path: &Path, fixed: &mut Vec<String>) -> Result<(), AppError> {
    if path.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(path)
        .map_err(|error| AppError::io("create doctor repair directory", path, error))?;
    fixed.push(path.display().to_string());
    Ok(())
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
