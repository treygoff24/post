use crate::channel::{channel_state_path, parse_channel_message, ChannelPaths, CHANNELS_DIR};
use crate::channel_state;
use crate::cli::{DoctorArgs, DoctorSeverityFilter};
use crate::command_result::CommandResult;
use crate::commands::schema::doctor_exit_codes;
use crate::cursor_state::{CURSORS_FILE, CURSORS_LOCK_FILE};
use crate::error::{AppError, AppResult, ErrorCode};
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

struct DoctorProjection {
    participant: serde_json::Value,
    pending: BTreeMap<String, usize>,
    /// Set when an explicit claim names a record that does not exist. That is
    /// a diagnosis for the reader to act on, not a store fault, so it rides as
    /// a field beside the report and never changes its status or exit code.
    missing: Option<super::participant::MissingReport>,
}

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
            let mut checks = checks;
            let projection = project(context, &mut checks);
            let output = report(context, checks, fixed, args.severity);
            return finish(output, projection, args.brief, pretty, 3);
        }
    }
    let mut checks = detect(context);
    let projection = project(context, &mut checks);
    let output = report(context, checks, fixed, args.severity);
    let exit_code = if output.count == 0 { 0 } else { 1 };
    finish(output, projection, args.brief, pretty, exit_code)
}

/// Emit the doctor result: the full JSON report by default, or a single
/// summary line under --brief. Exit codes are identical either way.
fn finish(
    output: DoctorOutput,
    projection: DoctorProjection,
    brief: bool,
    pretty: bool,
    exit_code: i32,
) -> AppResult<CommandResult> {
    let mut result = if brief {
        CommandResult::success(brief_line(
            &output,
            projection.missing.as_ref().map(|report| report.fix()),
        ))
    } else {
        let mut value = serde_json::to_value(&output).map_err(|error| {
            AppError::invalid_argument(format!("serialize doctor report: {error}"))
        })?;
        let object = value.as_object_mut().expect("doctor output is an object");
        object.insert("participant".to_owned(), projection.participant);
        if let Some(report) = projection.missing {
            object.insert("bound".to_owned(), serde_json::Value::Bool(false));
            object.insert(
                "participant_missing".to_owned(),
                serde_json::to_value(report).expect("missing report is JSON"),
            );
        }
        object.insert(
            "pending".to_owned(),
            serde_json::to_value(projection.pending).expect("pending map"),
        );
        CommandResult::json(&value, pretty)?
    };
    result.exit_code = exit_code;
    Ok(result)
}

fn project(context: &Context, checks: &mut Vec<DoctorCheck>) -> DoctorProjection {
    let mut missing = None;
    let resolved = match crate::participant::resolve(context) {
        Ok(resolved) => resolved,
        // A claim that names no record is reported as a field and its fix,
        // exit code untouched: doctor is the surface that diagnoses it.
        Err(error) if error.code == ErrorCode::ParticipantMissing => {
            missing = Some(super::participant::MissingReport::from_error(&error));
            crate::participant::Resolved::Unbound
        }
        Err(error) => {
            checks.push(DoctorCheck {
                id: "participant.binding.invalid".to_owned(),
                severity: DoctorSeverity::Error,
                path: context
                    .root
                    .join(crate::participant::PARTICIPANTS_DIR)
                    .display()
                    .to_string(),
                message: error.message,
                fixable: false,
                suggested_fix: error.suggested_fix,
            });
            crate::participant::Resolved::Unbound
        }
    };
    match &resolved {
        crate::participant::Resolved::Bound {
            participant,
            provenance,
        } => {
            let mut pending = BTreeMap::new();
            match super::inbox::visible_addresses(context, participant) {
                Ok(addresses) => {
                    for address in addresses {
                        let label = super::inbox::address_label(&address);
                        match crate::cursor_state::routing::provisional_pending_for(
                            context,
                            participant,
                            &address,
                        ) {
                            Ok(ids) => {
                                pending.insert(label, ids.len());
                            }
                            Err(error) => push_projection_error(
                                checks,
                                &label,
                                &crate::cursor_state::routing::routing_dir(context, &address),
                                error,
                            ),
                        }
                    }
                }
                Err(error) => push_projection_error(checks, "addresses", &context.root, error),
            }
            DoctorProjection {
                participant: serde_json::json!({
                    "status": "bound",
                    "id": participant.id,
                    "provenance": provenance.as_str(),
                    "workspace": participant.workspace,
                    "lineage": participant.lineage,
                }),
                pending,
                missing,
            }
        }
        crate::participant::Resolved::Unbound => {
            let mut pending = BTreeMap::new();
            match context.load_rooms() {
                Ok(rooms) => {
                    for room in rooms.into_keys() {
                        let address = crate::participant::Address {
                            kind: crate::participant::AddressKind::Workspace,
                            name: room,
                        };
                        let label = super::inbox::address_label(&address);
                        match crate::cursor_state::routing::pending_count(context, &address) {
                            Ok(count) => {
                                pending.insert(label, count);
                            }
                            Err(error) => push_projection_error(
                                checks,
                                &label,
                                &crate::cursor_state::routing::routing_dir(context, &address),
                                error,
                            ),
                        }
                    }
                }
                Err(error) => {
                    push_projection_error(checks, "rooms", &context.root.join("rooms.json"), error)
                }
            }
            let participant = match &missing {
                Some(report) => serde_json::json!({
                    "status": "missing",
                    "fix": format!("run: {}", report.fix()),
                }),
                None => serde_json::json!({
                    "status": "unbound",
                    "fix": "run: post participant bind"
                }),
            };
            DoctorProjection {
                participant,
                pending,
                missing,
            }
        }
    }
}

fn push_projection_error(checks: &mut Vec<DoctorCheck>, label: &str, path: &Path, error: AppError) {
    checks.push(DoctorCheck {
        id: format!("projection.{}", label.replace(':', ".")),
        severity: DoctorSeverity::Error,
        path: path.display().to_string(),
        message: error.message,
        fixable: false,
        suggested_fix: error.suggested_fix,
    });
}

/// The one-line --brief summary. Healthy mailboxes name how many checks ran;
/// anything else points back at the full report for the detail.
fn brief_line(output: &DoctorOutput, missing_fix: Option<String>) -> String {
    // A filtered report names what it hides, so "N findings" is never read as
    // the list a reader would get from a plain `post doctor`.
    let hidden = match (output.filtered_out, output.severity_filter.as_deref()) {
        (Some(hidden), Some(threshold)) if hidden > 0 => {
            format!(", {hidden} of them hidden by --severity {threshold}")
        }
        _ => String::new(),
    };
    // A claim that names no record is not a finding, but a reader who only
    // sees this line must still learn it and how to repair it.
    let claim = missing_fix
        .map(|fix| {
            format!("; POST_PARTICIPANT or the session's binding names no record (fix: {fix})")
        })
        .unwrap_or_default();
    if output.count == 0 {
        format!("post doctor: ok ({} checks){claim}\n", output.checks.len())
    } else {
        format!(
            "post doctor: {} findings{hidden} (run post doctor for detail){claim}\n",
            output.count
        )
    }
}

fn report(
    context: &Context,
    mut checks: Vec<DoctorCheck>,
    fixed: Vec<String>,
    threshold: Option<DoctorSeverityFilter>,
) -> DoctorOutput {
    // Info checks (owner state etc.) are surface, not findings: they never
    // flip ok/status/count, so a healthy configured mailbox still exits 0.
    let is_finding = |check: &DoctorCheck| check.severity != DoctorSeverity::Info;
    // `ok`, `status`, `count`, and the exit code describe every check, whatever
    // the threshold hides: `--severity error` is a lens on the list, and an
    // agent that gates on it must still be told about the warnings it hides.
    let findings = checks.iter().filter(|check| is_finding(check)).count();
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
    if let Some(threshold) = threshold {
        checks.retain(|check| match threshold {
            DoctorSeverityFilter::Warn => check.severity != DoctorSeverity::Info,
            DoctorSeverityFilter::Error => check.severity == DoctorSeverity::Error,
        });
    }
    // Findings the threshold hid: `count` = the findings listed + this.
    let filtered_out =
        threshold.map(|_| findings - checks.iter().filter(|check| is_finding(check)).count());
    DoctorOutput {
        ok: findings == 0,
        status: status.to_owned(),
        root: context.root.display().to_string(),
        count: findings,
        checks,
        fixed,
        exit_codes: doctor_exit_codes(),
        severity_filter: threshold.map(|threshold| threshold.as_str().to_owned()),
        filtered_out,
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
    detect_routing_receipts(context, &mut checks);
    detect_rename_journal(context, &mut checks);
    detect_bridge_attention(context, &mut checks);
    detect_skill_drift(context, &mut checks);
    detect_claude_hook_drift(context, &mut checks);

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
                    let participants = crate::participant::list(context).unwrap_or_default();
                    for (key, profile) in &profiles {
                        let mut cleaned = profile.clone();
                        if let Some(id) = crate::profile::participant_of_key(key) {
                            // Participant-keyed: validate against the participant's
                            // reply address (its workspace, else its id).
                            let own_room = participants
                                .iter()
                                .find(|record| record.id == id)
                                .and_then(|record| record.workspace.clone())
                                .unwrap_or_else(|| id.to_owned());
                            if crate::profile::drop_invalid_fields(
                                &mut cleaned,
                                &own_room,
                                rooms,
                                owner_room,
                            ) {
                                checks.push(check(
                                    &format!("profiles.{key}.inert"),
                                    DoctorSeverity::Warning,
                                    &profiles_path,
                                    "stored profile entry no longer validates and will not stamp or render",
                                    false,
                                    "Re-run `post profile set` as that participant, or remove the entry.",
                                ));
                            }
                            continue;
                        }
                        // Legacy workspace-keyed entry: shared by every participant
                        // bound to that workspace, so it never stamps (2026-09-22).
                        let bound: Vec<&str> = participants
                            .iter()
                            .filter(|record| record.workspace.as_deref() == Some(key.as_str()))
                            .map(|record| record.id.as_str())
                            .collect();
                        // Never auto-migrated: `participant::list` skips malformed
                        // records, so "sole participant" cannot be proven from it and
                        // a persona could land on the wrong survivor. Each participant
                        // claims its own; the first `set` from that workspace retires
                        // the entry.
                        let message = match bound.len() {
                            0 => "legacy workspace-keyed profile no longer stamps and no participant is bound to that workspace".to_owned(),
                            n => format!(
                                "legacy workspace-keyed profile no longer stamps ({n} participant(s) bound to '{key}' render without it)"
                            ),
                        };
                        checks.push(check(
                            &format!("profiles.{key}.legacy_workspace_key"),
                            DoctorSeverity::Warning,
                            &profiles_path,
                            &message,
                            false,
                            "Each participant runs `post profile set --name ... --pfp ...`; the first set from that workspace retires the legacy entry. Or remove the entry by hand.",
                        ));
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
            // A room's inbox/ is created by the first send to it and its
            // legacy read/ is history only, so an absent directory is the
            // normal state of a room nobody has written to yet, not a fault.
            // Doctor used to flag both as errors, which kept every healthy
            // store "broken" and taught agents to ignore the report.
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
    let root = context.root.join(crate::participant::PARTICIPANTS_DIR);
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if id == "by-session" || !entry.path().is_dir() {
                continue;
            }
            if let Err(error) = crate::participant::load(context, &id) {
                checks.push(check(
                    &format!("participant.{id}.invalid"),
                    DoctorSeverity::Error,
                    &entry.path().join("participant.json"),
                    &error.message,
                    false,
                    "Repair the participant record by hand; post skips it for routing until valid.",
                ));
            }
            if let Some(participant) = crate::participant::load(context, &id).ok().flatten() {
                if let Err(error) = crate::channel_state::validate_channels_file(&participant) {
                    checks.push(check(
                        &format!("participant.{id}.channels_invalid"),
                        DoctorSeverity::Error,
                        &participant.dir.join("channels.json"),
                        &error.message,
                        false,
                        "Restore or repair this participant's channels.json from a backup; other participants remain usable.",
                    ));
                }
                // Channel operations fail loud (config_invalid) on a malformed
                // membership-starts.json, as they do on channels.json; doctor
                // names the file so the defect is not blamed on channels.json.
                if let Err(error) =
                    crate::channel_state::validate_membership_starts_file(&participant)
                {
                    checks.push(check(
                        &format!("participant.{id}.membership_starts_invalid"),
                        DoctorSeverity::Error,
                        &participant
                            .dir
                            .join(crate::channel_state::MEMBERSHIP_STARTS_FILE),
                        &error.message,
                        false,
                        "Repair this participant's membership-starts.json by hand or restore it from a backup; other participants remain usable. Removing it is a last resort: every joined channel then starts at the participant's created time, so messages between that time and each join read as unread again.",
                    ));
                }
                // Runtime reads degrade an unusable cursors.json to an empty
                // snapshot and re-report consumed history as unread: a doorbell
                // built on that participant then rings as if every backlog
                // message were new. The degrade is deliberate (fail open), so
                // doctor warns rather than repairs -- and `--fix` never touches
                // cursor state.
                //
                // The suggested fix is deliberately NOT "move it aside": the
                // file is the participant's only record of what it has already
                // read, so discarding it re-reports the whole backlog and loses
                // the evidence of whatever made it unreadable. Post has no safe
                // automated repair to offer, and this says so rather than
                // turning data loss into the recipe.
                if let Some(reason) = crate::cursor_state::participant_cursor_defect(&participant) {
                    checks.push(check(
                        &format!("participant.{id}.cursors_unusable"),
                        DoctorSeverity::Warning,
                        &participant.dir.join(crate::cursor_state::CURSORS_FILE),
                        &format!(
                            "participant cursor state is unusable ({reason}); reads report already-consumed history as unread until it is repaired"
                        ),
                        false,
                        "Post has no safe automated repair for this: keep the file as evidence, and either repair it in place after copying it for forensics (fix the JSON, keep the existing ids) or restore a known-good backup of it. Discarding it is not a repair -- it destroys the only record of what this participant has read.",
                    ));
                }
            }
        }
    }
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
    // One line for all of them. A store that has run for weeks holds
    // hundreds of expired sessions (1,217 of the Mac's 1,335 checks were one
    // Info line per stale participant), and none of them is a fault.
    let mut expired = 0usize;
    let mut no_lease = 0usize;
    for participant in participants {
        if participant.state(now) != crate::participant::ParticipantState::Stale {
            continue;
        }
        if participant.last_seen.is_none() {
            no_lease += 1;
        } else {
            expired += 1;
        }
    }
    let total = expired + no_lease;
    if total > 0 {
        // The prune numbers come from the same plan `post participant gc`
        // makes, so the two never disagree about what could go.
        let prune = match super::participant_gc::plan(context, now) {
            Ok(plan) => {
                let (deleted, archived) = plan.counts();
                format!(
                    "; `post participant gc` would delete {deleted} and archive {archived} participant record(s)"
                )
            }
            Err(_) => String::new(),
        };
        let auto = super::participant_auto_gc::last_run_summary(context);
        checks.push(check(
            "participants.stale",
            DoctorSeverity::Info,
            &root,
            &format!(
                "{total} participant(s) are inactive for new recipient selection ({expired} with an expired lease, {no_lease} with no lease record); mail already frozen to them is kept, not reassigned{prune}; {auto}"
            ),
            false,
            "Nothing to repair. Run `post participant gc` to preview the prune (a dry run), then `post participant gc --apply` to do it; a participant with unread or pending mail is kept.",
        ));
    }
}

fn detect_routing_receipts(context: &Context, checks: &mut Vec<DoctorCheck>) {
    for address in crate::cursor_state::routing::store_addresses(context) {
        let directory = crate::cursor_state::routing::routing_dir(context, &address);
        if let Ok(entries) = fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                if let Err(error) = crate::cursor_state::routing::receipt(context, &address, id) {
                    checks.push(check(
                        &format!(
                            "routing.receipt.{}.{}.{}.invalid",
                            address.kind.as_str(),
                            address.name,
                            id
                        ),
                        DoctorSeverity::Error,
                        &path,
                        &error.message,
                        false,
                        "Repair or remove the corrupt receipt after reconciling it with the canonical message.",
                    ));
                }
            }
        }
        match crate::cursor_state::routing::held_ids(context, &address) {
            Ok(ids) if !ids.is_empty() => checks.push(check(
                &format!(
                    "routing.held.{}.{}",
                    address.kind.as_str(),
                    address.name
                ),
                DoctorSeverity::Warning,
                &crate::cursor_state::routing::inbox_path(context, &address),
                &format!("held mail ids: {}", ids.join(", ")),
                false,
                "Review the blocking route; do not bypass it. Mail remains held until policy changes.",
            )),
            Ok(_) => {}
            Err(error) => checks.push(check(
                &format!(
                    "routing.held.{}.{}.unknown",
                    address.kind.as_str(),
                    address.name
                ),
                DoctorSeverity::Error,
                &crate::cursor_state::routing::inbox_path(context, &address),
                &error.message,
                false,
                "Repair the route configuration, then rerun doctor.",
            )),
        }
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
        if let Err(error) = crate::channel_archive::load(&paths) {
            checks.push(check(
                &format!("channel.{name}.archive_invalid"),
                DoctorSeverity::Error,
                &crate::channel_archive::archive_path(&paths),
                &error.message,
                false,
                "Restore a valid archive.json by hand; until then the channel lists as live. Nothing is deleted.",
            ));
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
                if message_path.extension().and_then(|value| value.to_str()) == Some("emote") {
                    let result =
                        fs::read(&message_path)
                            .map_err(|_| "read-error")
                            .and_then(|bytes| {
                                crate::emote::decode(
                                    &bytes,
                                    message_path.file_stem().and_then(|s| s.to_str()),
                                )
                            });
                    if let Err(rule) = result {
                        checks.push(check(
                            "channels.unreadable_emote",
                            DoctorSeverity::Warning,
                            &message_path,
                            &format!("unreadable emote: {rule}"),
                            false,
                            "Inspect the emote envelope; nothing is moved or deleted.",
                        ));
                    }
                } else if message_path.extension().and_then(|value| value.to_str()) != Some("msg") {
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
                &format!("legacy room cursors.json cannot be used ({reason}); participant reads ignore it"),
                false,
                "Inspect the read-only legacy room state by hand; participant cursors live under participants/<id>/cursors.json.",
            ));
        } else {
            checks.push(check(
                &format!("legacy_room_state.{room}.cursors"),
                DoctorSeverity::Info,
                &cursor_path,
                "legacy room cursors.json is read-only; participant reads do not consume or update it",
                false,
                "Keep it for rollback evidence or archive it deliberately after migration acceptance.",
            ));
        }
    }

    let legacy_read = room_dir.join("read");
    if legacy_read.is_dir() {
        checks.push(check(
            &format!("legacy_room_state.{room}.read"),
            DoctorSeverity::Info,
            &legacy_read,
            "legacy room read/ history is read-only and is never listed as participant unread mail",
            false,
            "Keep it as consumed history; routed canonical mail remains in inbox/.",
        ));
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
                "legacy channel-state.json is read-only; participant channel seen-sets live under participants/<id>/cursors.json",
                false,
                "Keep it for legacy readers; participant writes never import or replace it.",
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
    // Room mailboxes are not created here any more: doctor no longer reports
    // a missing `<room>/{inbox,read}` (the first send creates the inbox), and
    // a repair that materializes empty directories nobody asked for would
    // also race a concurrent `rooms rename` for no benefit.
    create_dir(&context.root.join("archive"), fixed)?;
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

/// One "something is stuck" item from the bridge's `bridge/health.json`
/// `attention` list: a letter it refused or could not relay, an inbound letter
/// it quarantined, a name collision. The bridge writes the fix with the item.
pub(super) struct BridgeAttention {
    pub kind: String,
    pub id: Option<String>,
    pub summary: String,
    pub fix: String,
}

/// How much of `bridge/health.json` doctor and `who` will read.
const BRIDGE_HEALTH_MAX_BYTES: u64 = 1 << 20;

/// What `bridge/health.json` says needs attention, and whether the file could
/// be read at all.
pub(super) struct BridgeAttentionReport {
    pub items: Vec<BridgeAttention>,
    /// Why the health file cannot be read. Set only on a bridged host (one
    /// with a `bridge/config.json`): there a bridge that cannot report is not
    /// the same as a bridge with nothing to report, and silence would hide
    /// every stuck letter. An unbridged host has no health file to read and
    /// is healthy.
    pub unreadable: Option<String>,
}

/// The fix shown with an unreadable bridge health file, in doctor and `who`.
pub(super) const BRIDGE_HEALTH_FIX: &str = "Check that the bridge is running (`post-bridge status` on this host); it rewrites bridge/health.json every tick. A bridge older than the attention list needs updating.";

/// Why a bridged host's health file cannot be trusted, as one sentence for a
/// doctor message or a `who` line.
pub(super) fn bridge_health_message(reason: &str) -> String {
    format!(
        "this host has a bridge (bridge/config.json) but bridge/health.json {reason}, so stuck or refused letters would not show up here"
    )
}

/// The bridge's attention items and whether its health file was readable.
/// Nothing here is an error: doctor and `who` report what they find.
pub(super) fn bridge_attention(context: &Context) -> BridgeAttentionReport {
    // A config that exists but cannot be trusted still means a bridge was set
    // up here; only a conclusively absent config is an unbridged host.
    let bridged = !matches!(crate::bridge_topology::load_config(context), Ok(None));
    match read_bridge_attention(context) {
        Ok(items) => BridgeAttentionReport {
            items,
            unreadable: None,
        },
        Err(reason) => BridgeAttentionReport {
            items: Vec::new(),
            unreadable: bridged.then_some(reason),
        },
    }
}

/// Why a valid health file is unusable when it lacks the `attention` key.
const NO_ATTENTION_REASON: &str =
    "has no attention list (written by a bridge older than that format)";

fn read_bridge_attention(context: &Context) -> Result<Vec<BridgeAttention>, String> {
    let path = crate::bridge_topology::bridge_dir(context).join("health.json");
    let bytes = crate::bridge_topology::read_regular(&path, BRIDGE_HEALTH_MAX_BYTES)
        .map_err(|error| format!("cannot be read ({error})"))?
        .ok_or_else(|| "does not exist".to_owned())?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| "is not valid JSON".to_owned())?;
    let items = match value.get("attention") {
        Some(serde_json::Value::Array(items)) => items,
        Some(_) => return Err("has an attention entry that is not a list".to_owned()),
        None => return Err(NO_ATTENTION_REASON.to_owned()),
    };
    let text = |item: &serde_json::Value, key: &str| {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    Ok(items
        .iter()
        .filter(|item| item.is_object())
        .map(|item| {
            let kind = text(item, "kind").unwrap_or_else(|| "unknown".to_owned());
            BridgeAttention {
                summary: text(item, "summary").unwrap_or_else(|| kind.clone()),
                id: text(item, "id"),
                fix: text(item, "fix").unwrap_or_default(),
                kind,
            }
        })
        .collect())
}

const LEGACY_UPDATE: &str = "Update the bridge so it writes the attention list (it then names each letter with its own fix).";

/// A pre-attention bridge's health file carries counters instead of an
/// attention list. Returns one `(id, message, fix)` per non-zero counter; zero,
/// absent, or wrongly typed counters give nothing. `None` when the file cannot
/// be read as a JSON object.
fn legacy_bridge_counters(context: &Context) -> Option<Vec<(String, String, String)>> {
    let path = crate::bridge_topology::bridge_dir(context).join("health.json");
    let bytes = crate::bridge_topology::read_regular(&path, BRIDGE_HEALTH_MAX_BYTES)
        .ok()
        .flatten()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    value.as_object()?;
    // A count: a non-negative integer, or a list's length (`faults` has been both).
    let count = |value: Option<&serde_json::Value>| -> u64 {
        match value {
            Some(serde_json::Value::Number(number)) => number.as_u64().unwrap_or(0),
            Some(serde_json::Value::Array(items)) => items.len() as u64,
            _ => 0,
        }
    };
    let mut findings = Vec::new();
    let mut add = |id: &str, message: String, fix: String| {
        findings.push((format!("bridge.legacy.{id}"), message, fix));
    };
    let held = count(value.get("held"));
    if held > 0 {
        add(
            "held",
            format!("the bridge reports {held} held letter(s) (legacy counter)"),
            format!("Held letters are recorded as status `held` receipts in the bridge's relay repository (BRIDGE_REPO, receipts/<host>/<room>/<id>.json); read the bridge log for each reason. {LEGACY_UPDATE}"),
        );
    }
    let quarantined = count(value.get("quarantined"));
    if quarantined > 0 {
        add(
            "quarantined",
            format!("the bridge reports {quarantined} quarantined inbound letter(s) (legacy counter)"),
            format!("Forensic copies sit under bridge/quarantine/<host>/<room>/ in this store; inspect them, then fix the sender or the room route. {LEGACY_UPDATE}"),
        );
    }
    if let Some(serde_json::Value::Array(ids)) = value.get("outbound_unrelayable") {
        let ids: Vec<&str> = ids.iter().filter_map(serde_json::Value::as_str).collect();
        if !ids.is_empty() {
            add(
                "outbound_unrelayable",
                format!(
                    "the bridge cannot relay {} outbound letter(s) (legacy list, at most 20 shown): {}",
                    ids.len(),
                    ids.join(", ")
                ),
                format!("Find each id's .mail file in its room's inbox/read directory in this store and check why it cannot be relayed (unknown host, oversized, malformed). {LEGACY_UPDATE}"),
            );
        }
    }
    let channels_quarantined = count(value.get("channels").and_then(|c| c.get("quarantined")));
    if channels_quarantined > 0 {
        add(
            "channels_quarantined",
            format!("the bridge reports {channels_quarantined} quarantined channel message(s) (legacy counter)"),
            format!("Read the bridge log for the channel and message ids it quarantined. {LEGACY_UPDATE}"),
        );
    }
    let rejected = count(value.get("pmail").and_then(|p| p.get("rejected")));
    if rejected > 0 {
        add(
            "pmail_rejected",
            format!("the bridge reports {rejected} rejected typed (pmail) letter(s) (legacy counter)"),
            format!("Read the bridge log for each rejection reason and resend the letter once fixed. {LEGACY_UPDATE}"),
        );
    }
    let faults = count(value.get("local_held").and_then(|l| l.get("faults")));
    if faults > 0 {
        add(
            "local_held_faults",
            format!("the bridge reports {faults} local-hold fault(s) (legacy counter)"),
            format!("Read the bridge log for the fault detail. {LEGACY_UPDATE}"),
        );
    }
    Some(findings)
}

/// At most this many attention items become individual warnings; the rest are
/// one summary line, so a bridge with hundreds of stuck letters cannot bury
/// the rest of the report.
const ATTENTION_SHOWN: usize = 25;

fn detect_bridge_attention(context: &Context, checks: &mut Vec<DoctorCheck>) {
    let BridgeAttentionReport { items, unreadable } = bridge_attention(context);
    let path = crate::bridge_topology::bridge_dir(context).join("health.json");
    if let Some(reason) = unreadable {
        let legacy = (reason == NO_ATTENTION_REASON)
            .then(|| legacy_bridge_counters(context))
            .flatten();
        let message = if legacy.is_some() {
            "bridge/health.json was written by a bridge older than the attention list; its legacy counters follow, but it cannot name individual stuck letters".to_owned()
        } else {
            bridge_health_message(&reason)
        };
        checks.push(check(
            "bridge.health_unreadable",
            DoctorSeverity::Warning,
            &path,
            &message,
            false,
            BRIDGE_HEALTH_FIX,
        ));
        for (id, message, fix) in legacy.unwrap_or_default() {
            checks.push(check(
                &id,
                DoctorSeverity::Warning,
                &path,
                &message,
                false,
                &fix,
            ));
        }
    }
    if items.is_empty() {
        return;
    }
    let total = items.len();
    for item in items.into_iter().take(ATTENTION_SHOWN) {
        let id = match &item.id {
            Some(id) => format!("bridge.attention.{}.{id}", item.kind),
            None => format!("bridge.attention.{}", item.kind),
        };
        let fix = if item.fix.trim().is_empty() {
            "The bridge gave no fix for this item; read bridge/health.json and run `post-bridge status` on this host."
                .to_owned()
        } else {
            item.fix
        };
        checks.push(check(
            &id,
            DoctorSeverity::Warning,
            &path,
            &item.summary,
            false,
            &fix,
        ));
    }
    if total > ATTENTION_SHOWN {
        checks.push(check(
            "bridge.attention.more",
            DoctorSeverity::Warning,
            &path,
            &format!(
                "{} more bridge attention item(s) are not listed here",
                total - ATTENTION_SHOWN
            ),
            false,
            "Read the `attention` list in bridge/health.json for the rest.",
        ));
    }
}

/// How much of an installed hook file doctor will hash; a longer file is not
/// the shipped one.
const HOOK_MAX_BYTES: u64 = 1 << 20;

/// `Some(true)` when the file at `path` (symlinks followed) hashes to
/// `expected`, `Some(false)` when it differs or is too large, `None` when it
/// does not exist or cannot be read.
fn hook_file_matches(path: &Path, expected: &str) -> Option<bool> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(HOOK_MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > HOOK_MAX_BYTES {
        return Some(false);
    }
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(digest == expected)
}

/// The Claude installer copies the adapter and its core into
/// `~/.claude/hooks/`, and Claude Code runs those copies, so a host that never
/// re-ran the installer keeps old hook behaviour. Compare the copies with the
/// hashes this binary's skill manifest holds. No adapter means hooks are not
/// installed here: no finding.
fn detect_claude_hook_drift(context: &Context, checks: &mut Vec<DoctorCheck>) {
    let dir = context.home.join(".claude/hooks");
    let adapter = dir.join("post-claude-mail.mjs");
    let core = dir.join("mail-hook-core.mjs");
    if !adapter.is_file() {
        return;
    }
    let (Some(adapter_sha), Some(core_sha)) = (
        super::contract::manifest_sha256("hooks/claude-mail.mjs"),
        super::contract::manifest_sha256("hooks/mail-hook-core.mjs"),
    ) else {
        return;
    };
    let mut stale = Vec::new();
    if hook_file_matches(&adapter, adapter_sha) != Some(true) {
        stale.push("post-claude-mail.mjs differs from the adapter this binary ships".to_owned());
    }
    match hook_file_matches(&core, core_sha) {
        Some(true) => {}
        None if !core.exists() => stale.push(
            "mail-hook-core.mjs is missing beside the adapter (the old single-file layout)"
                .to_owned(),
        ),
        _ => stale.push("mail-hook-core.mjs differs from the core this binary ships".to_owned()),
    }
    if stale.is_empty() {
        return;
    }
    checks.push(check(
        "hooks.claude_drift",
        DoctorSeverity::Warning,
        &dir,
        &format!(
            "the installed Claude mail hooks are stale: {}",
            stale.join("; ")
        ),
        false,
        "Re-run the installer from a post checkout at the commit `post --version` reports: node <checkout>/skills/post/hooks/install-claude-hooks.mjs ~/.claude/settings.json",
    ));
}

/// The served skill checkout, where the installer puts it by default.
const SERVED_SKILL: &str = ".agents/skill-library/post";

/// Compare the served skill with the copy this binary was built against
/// (`post contract skill-manifest --verify`). Drift means the agents' prose
/// and hooks disagree with the installed binary: they document flags the
/// binary lacks, or miss ones it has. An absent served path is not a finding
/// (a host without the skill installed), and neither is a rendered copy the
/// binary cannot fully verify.
fn detect_skill_drift(context: &Context, checks: &mut Vec<DoctorCheck>) {
    let served = context.home.join(SERVED_SKILL);
    if fs::symlink_metadata(&served).is_err() {
        return;
    }
    match super::contract::verify_served(&served) {
        Ok(verdict) if verdict.verdict == "drift" => {
            let mut detail = Vec::new();
            if !verdict.mismatched.is_empty() {
                detail.push(format!("changed: {}", verdict.mismatched.join(", ")));
            }
            if !verdict.missing.is_empty() {
                detail.push(format!("missing: {}", verdict.missing.join(", ")));
            }
            if !verdict.extra.is_empty() {
                detail.push(format!("extra: {}", verdict.extra.join(", ")));
            }
            checks.push(check(
                "skill.drift",
                DoctorSeverity::Warning,
                &served,
                &format!(
                    "the served post skill differs from the one this binary was built with ({})",
                    detail.join("; ")
                ),
                false,
                "The skill and the binary come from different commits. Sync the served skill checkout to the commit `post --version` reports, or install the newer binary with scripts/install-post.sh <commit>, whichever is behind.",
            ));
        }
        Ok(_) => {}
        Err(error) => checks.push(check(
            "skill.unverifiable",
            DoctorSeverity::Info,
            &served,
            &format!(
                "the served post skill could not be checked: {}",
                error.message
            ),
            false,
            "Check that the path is a readable directory or a symlink to one.",
        )),
    }
}

/// A standing `rename-journal.json` is an interrupted `post rooms rename`:
/// the store may be half-moved until the same rename is resumed.
fn detect_rename_journal(context: &Context, checks: &mut Vec<DoctorCheck>) {
    let path = context.root.join(crate::mailbox::RENAME_JOURNAL_FILE);
    match crate::mailbox::read_rename_journal(context) {
        Ok(None) => {}
        Ok(Some(journal)) => {
            let fix = crate::mailbox::resume_command(&journal.old, &journal.new);
            checks.push(check(
                "rooms.rename_interrupted",
                DoctorSeverity::Error,
                &path,
                &format!(
                    "a rename of '{}' to '{}' started at {} did not finish; the room's mailbox and live state may be split between the two names",
                    journal.old, journal.new, journal.started_at
                ),
                false,
                &format!("Resume it with `{fix}`."),
            ));
            let old_home = context.root.join(&journal.old);
            if old_home.exists() && context.root.join(&journal.new).exists() {
                checks.push(check(
                    "rooms.rename_old_recreated",
                    DoctorSeverity::Error,
                    &old_home,
                    &format!(
                        "the old mailbox '{}' was recreated after the interrupted move to '{}'; the resume refuses until it is gone",
                        journal.old, journal.new
                    ),
                    false,
                    &format!(
                        "Move each file under {} into the matching place under {} by hand (or remove it if it is not mail), remove the old directory, then run `{fix}`.",
                        old_home.display(),
                        context.root.join(&journal.new).display()
                    ),
                ));
            }
        }
        Err(error) => checks.push(check(
            "rooms.rename_journal_invalid",
            DoctorSeverity::Error,
            &path,
            &error.message,
            false,
            "Inspect the mail root to see which rename was interrupted, finish it by hand, then delete the journal.",
        )),
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
