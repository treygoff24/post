use super::{schema::doctor_exit_codes, CommandResult, Context};
use crate::cli::DoctorArgs;
use crate::error::{AppError, AppResult};
use crate::output::{self, DoctorCheck, DoctorOutput, DoctorSeverity};
use crate::{parse_mail, validate_component, RulesConfig};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub fn run(context: &Context, args: DoctorArgs, pretty: bool) -> AppResult<CommandResult> {
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
            return Ok(CommandResult {
                stdout: output::json(&output, pretty)?,
                exit_code: 3,
            });
        }
    }
    let checks = detect(context);
    let output = report(context, checks, fixed);
    let exit_code = if output.count == 0 { 0 } else { 1 };
    Ok(CommandResult {
        stdout: output::json(&output, pretty)?,
        exit_code,
    })
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
    checks.sort_by(|left, right| left.id.cmp(&right.id).then(left.path.cmp(&right.path)));
    checks
}

fn detect_rooms(
    context: &Context,
    path: &Path,
    checks: &mut Vec<DoctorCheck>,
) -> Option<BTreeMap<String, String>> {
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
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, String>>(&bytes).ok())
    {
        Some(rooms) if !rooms.is_empty() => {
            for (name, value) in &rooms {
                if let Err(reason) = validate_component(name) {
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

fn detect_rules(
    path: &Path,
    rooms: Option<&BTreeMap<String, String>>,
    checks: &mut Vec<DoctorCheck>,
) {
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
                } else if let Err(error) = parse_mail(&mail_path) {
                    checks.push(check(
                        "state.malformed_mail",
                        DoctorSeverity::Error,
                        &mail_path,
                        &error.message,
                        false,
                        "Restore a valid envelope/body separator or move the file aside by hand; nothing is deleted.",
                    ));
                }
            }
        }
    }
}

fn apply_fixes(context: &Context, fixed: &mut Vec<String>) -> Result<(), AppError> {
    create_dir(&context.root, fixed)?;
    if context.write_default_if_missing("rooms.json", super::super::DEFAULT_ROOMS_JSON)? {
        fixed.push(context.root.join("rooms.json").display().to_string());
    }
    if context.write_default_if_missing("rules.json", super::super::DEFAULT_RULES_JSON)? {
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
