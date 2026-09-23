use crate::cli::{RoomsAddArgs, RoomsArgs, RoomsCommand, RoomsRenameArgs, RoomsSetPathArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{
    read_rename_journal, resume_command, shell_quote, validate_new_room_name, Context,
    RenameJournal, RENAME_JOURNAL_FILE,
};
use crate::model::{RoomMap, RulesConfig};
use crate::output::{RoomOutput, RoomsOutput, RoomsRenameOutput, RoomsSetPathOutput};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) fn run(context: &Context, args: RoomsArgs, pretty: bool) -> AppResult<CommandResult> {
    match args.command {
        Some(RoomsCommand::Add(args)) => add(context, args, pretty),
        Some(RoomsCommand::SetPath(args)) => set_path(context, args, pretty),
        Some(RoomsCommand::Rename(args)) => rename(context, args, pretty),
        None => list(context, pretty),
    }
}

fn list(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    render(&rooms, &rules, pretty)
}

fn add(context: &Context, args: RoomsAddArgs, pretty: bool) -> AppResult<CommandResult> {
    validate_new_room_name(&args.name).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room name '{}' is invalid: {reason}", args.name),
            "Pass a single path-safe room name without '/' or '\\'.",
        )
        .input(args.name.clone())
        .reason(reason)
    })?;
    // One namespace decision spans participants/lineages and rooms. Always
    // acquire the participant lock first, then the rooms lock, so identity
    // creation can keep using only the participant lock without deadlock.
    let _participant_lock = crate::participant::lock(context)?;
    let _lock = context.lock_rooms()?;
    if crate::lineage::load(context, &args.name)?.is_some() {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "room name '{}' is already used by an existing lineage",
                args.name
            ),
            "Choose a room name that does not collide with an existing lineage.",
        )
        .input(args.name)
        .reason("room names cannot collide with existing lineages"));
    }

    let mut rooms = context.load_rooms()?;
    if let Some(existing_name) = rooms
        .keys()
        .find(|name| name.eq_ignore_ascii_case(&args.name))
    {
        let placeholders = placeholder_hosts(context, &rooms);
        if let Some(host) = placeholders.get(existing_name.as_str()) {
            return Err(remote_duplicate_error(RemoteDuplicate {
                context,
                rooms: &rooms,
                placeholders: &placeholders,
                existing_name,
                requested: &args.name,
                host,
                fix_for: &|candidate| {
                    format!(
                        "post rooms add {} {}",
                        shell_quote(candidate),
                        shell_quote(&args.path)
                    )
                },
                retry: "`post rooms add`",
            }));
        }
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "room '{}' is already registered as '{existing_name}' under ASCII case folding",
                args.name
            ),
            format!("Choose a new room name; if the existing path is wrong, run `post rooms set-path {existing_name} <path>`."),
        )
        .input(args.name)
        .room(existing_name)
        .reason("duplicate room name under ASCII case folding"));
    }

    let (expanded, canonical) = validate_workspace_path(context, &args.path)?;
    let warnings =
        ensure_workspace_unclaimed(context, &rooms, &expanded, &canonical, &args.path, None)?;

    rooms.insert(args.name.clone(), args.path);
    let rules = context.load_rules(&rooms)?;
    if let Some(rule) = rules.blocked.iter().find(|rule| rule.targets(&args.name)) {
        return Err(AppError::new(
            ErrorCode::BlockedRoute,
            format!(
                "room '{}' cannot be registered because a route to it is blocked: {}",
                args.name, rule.reason
            ),
            "Do not route around this block. Ask the human operator to review rules.json.",
        )
        .input(args.name)
        .reason(rule.reason.clone())
        .rule(rule.clone()));
    }

    let result = render(&rooms, &rules, pretty)?;
    context.write_rooms(&rooms)?;
    for warning in warnings {
        eprintln!("{warning}");
    }
    Ok(result.registration_committed())
}

/// Re-point a local room's workspace (discovery) path in rooms.json. Only the
/// registry value changes: the room's mail and history live under its NAME in
/// the mail root, and participant records are never rewritten. Same locks,
/// lock order, and path validation as `add`. A remote placeholder is always
/// refused, in either direction: turning one into a local room (or a local
/// room into one) is a routing-ownership change (queued mail, bridge owner,
/// contest state), not a path edit.
fn set_path(context: &Context, args: RoomsSetPathArgs, pretty: bool) -> AppResult<CommandResult> {
    let _participant_lock = crate::participant::lock(context)?;
    let _lock = context.lock_rooms()?;
    let mut rooms = context.load_rooms()?;
    let Some(before) = rooms.get(&args.name).cloned() else {
        return Err(AppError::new(
            ErrorCode::UnknownRoom,
            format!("room '{}' is not registered", args.name),
            "List rooms with `post rooms`; register a new one with `post rooms add <name> <path>`.",
        )
        .input(args.name)
        .reason("set-path only re-points an existing room"));
    };
    let remote_refusal = |detail: &str| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room '{}' {detail}", args.name),
            "set-path never converts between a remote placeholder and a local room: that changes who owns routing for the name (queued mail, bridge owner, contest state). Leave the placeholder to the bridge.",
        )
        .input(args.name.clone())
        .room(args.name.clone())
        .reason("remote placeholders are refused")
    };
    if crate::output::remote_workspace(context, &args.name) {
        return Err(remote_refusal("is a remote placeholder"));
    }
    let (expanded, canonical) = validate_workspace_path(context, &args.path)?;
    let remote_root = fs::canonicalize(context.root.join("remote"))
        .unwrap_or_else(|_| context.root.join("remote"));
    if canonical.starts_with(&remote_root) || expanded.starts_with(context.root.join("remote")) {
        return Err(remote_refusal(
            "cannot be pointed into the remote placeholder tree",
        ));
    }
    let warnings = ensure_workspace_unclaimed(
        context,
        &rooms,
        &expanded,
        &canonical,
        &args.path,
        Some(&args.name),
    )?;
    let changed = before != args.path;
    let output = RoomsSetPathOutput {
        ok: true,
        room: args.name.clone(),
        before: before.clone(),
        after: args.path.clone(),
        changed,
        dry_run: args.dry_run,
    };
    let result = CommandResult::json(&output, pretty)?;
    for warning in warnings {
        eprintln!("{warning}");
    }
    if args.dry_run || !changed {
        if args.dry_run {
            eprintln!("post: dry run: rooms.json not changed");
        }
        return Ok(result);
    }
    rooms.insert(args.name, args.path);
    context.write_rooms(&rooms)?;
    Ok(result.registration_committed())
}

/// Rename a local room: the mailbox directory moves, every live reference to
/// the name is rewritten, and rooms.json commits last, inside the rollback.
/// History — archive letters, channel messages, the room's own cursor and
/// channel state contents — keeps the old name; only the live state that
/// KEYS or BINDS on the name changes (routing receipts bind
/// `workspace:<name>`, so they follow the room). `rename-journal.json`
/// records the intent so an interrupted rename can be resumed. Locks: the
/// rename lock (exclusive), then participant, then rooms.
fn rename(context: &Context, args: RoomsRenameArgs, pretty: bool) -> AppResult<CommandResult> {
    validate_new_room_name(&args.new).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room name '{}' is invalid: {reason}", args.new),
            "Pass a single path-safe room name without '/' or '\\'.",
        )
        .input(args.new.clone())
        .reason(reason)
    })?;
    // Lock order: rename (exclusive; the outermost store lock, see
    // RENAME_LOCK_FILE), then participant, then rooms. All are held through
    // the rooms.json commit.
    let _rename_lock = context.lock_rename(true)?;
    let _participant_lock = crate::participant::lock(context)?;
    let _lock = context.lock_rooms()?;
    let mut rooms = context.load_rooms()?;
    // A standing rename journal means an earlier rename of this pair was
    // interrupted. Only that same pair may run (it resumes); anything else
    // would plan against a half-moved store.
    let journal = read_rename_journal(context)?;
    if let Some(journal) = &journal {
        if journal.old != args.old || journal.new != args.new {
            return Err(journal_standing_error(context, journal));
        }
    }
    let resuming = journal.is_some();
    let old_home = context.root.join(&args.old);
    let new_home = context.root.join(&args.new);
    if resuming && !rooms.contains_key(&args.old) {
        return resume_committed_rename(context, &args, &rooms, &new_home, pretty);
    }
    let Some(stored_path) = rooms.get(&args.old).cloned() else {
        return Err(AppError::new(
            ErrorCode::UnknownRoom,
            format!("room '{}' is not registered", args.old),
            "List rooms with `post rooms`; register a new one with `post rooms add <name> <path>`.",
        )
        .input(args.old)
        .reason("rename only applies to an existing room"));
    };
    if crate::output::stored_path_remote_host_of(context, &stored_path).is_some() {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!("room '{}' is a remote placeholder", args.old),
            "rename never applies to a remote placeholder: the bridge owns it and republishes it. Rename the real room on its owning host.",
        )
        .input(args.old.clone())
        .room(args.old.clone())
        .reason("remote placeholders are refused"));
    }
    if args.new.eq_ignore_ascii_case(&args.old) {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "room name '{}' differs from '{}' only in ASCII case",
                args.new, args.old
            ),
            "Case-only renames are unsupported: pick a name that differs by more than case.",
        )
        .input(args.new.clone())
        .room(args.old.clone())
        .reason("case-only renames are unsupported"));
    }
    let placeholders = placeholder_hosts(context, &rooms);
    if let Some(existing_name) = rooms
        .keys()
        .find(|name| name.as_str() != args.old && name.eq_ignore_ascii_case(&args.new))
    {
        if let Some(host) = placeholders.get(existing_name.as_str()) {
            return Err(remote_duplicate_error(RemoteDuplicate {
                context,
                rooms: &rooms,
                placeholders: &placeholders,
                existing_name,
                requested: &args.new,
                host,
                fix_for: &|candidate| {
                    format!(
                        "post rooms rename {} {}",
                        shell_quote(&args.old),
                        shell_quote(candidate)
                    )
                },
                retry: "`post rooms rename`",
            }));
        }
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "room '{}' is already registered as '{existing_name}' under ASCII case folding",
                args.new
            ),
            format!("Choose a new room name; if the existing path is wrong, run `post rooms set-path {existing_name} <path>`."),
        )
        .input(args.new)
        .room(existing_name)
        .reason("duplicate room name under ASCII case folding"));
    }
    if crate::lineage::load(context, &args.new)?.is_some() {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "room name '{}' is already used by an existing lineage",
                args.new
            ),
            "Choose a room name that does not collide with an existing lineage.",
        )
        .input(args.new)
        .reason("room names cannot collide with existing lineages"));
    }
    let rules = context.load_rules(&rooms)?;
    if let Some(rule) = rules.blocked.iter().find(|rule| rule.targets(&args.new)) {
        return Err(AppError::new(
            ErrorCode::BlockedRoute,
            format!(
                "room '{}' cannot take the name because a route to it is blocked: {}",
                args.old, rule.reason
            ),
            "Do not route around this block. Ask the human operator to review rules.json.",
        )
        .input(args.new)
        .reason(rule.reason.clone())
        .rule(rule.clone()));
    }
    if let Some(rule) = rules
        .blocked
        .iter()
        .find(|rule| rule.from == args.old || rule.to == args.old)
    {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "rules.json has a rule naming '{}': {} -> {} ({})",
                args.old, rule.from, rule.to, rule.reason
            ),
            "rules.json is the human's file: update or remove the rule naming this room, then retry the rename.",
        )
        .input(args.old)
        .reason("a rules.json entry names the old room"));
    }
    // On resume, a present NEW with an absent OLD is the interrupted move
    // itself; the move is skipped and the rewrites re-planned from NEW.
    let mut moved_before = false;
    match fs::symlink_metadata(&new_home) {
        Ok(metadata) if resuming => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(AppError::config(
                    &new_home,
                    "the interrupted rename's target is a symlink or not a directory; repair it by hand before resuming",
                ));
            }
            match fs::symlink_metadata(&old_home) {
                Ok(_) => return Err(recreated_old_error(&args.old, &args.new, &old_home)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => moved_before = true,
                Err(error) => {
                    return Err(AppError::io("inspect room directory", &old_home, error));
                }
            }
        }
        Ok(_) => {
            return Err(AppError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "'{}' already exists in the mail root",
                    new_home.display()
                ),
                "Rename never merges two mailboxes. Remove or move the existing directory yourself if it is not mail state, then retry.",
            )
            .input(args.new)
            .reason("the new name's mailbox directory already exists"));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::io("inspect new room directory", &new_home, error)),
    }
    let owner_path = context.owner_json_path();
    match fs::symlink_metadata(&owner_path) {
        Ok(_) => {
            let owner = crate::mailbox::read_owner_file(&owner_path)?;
            if owner.room == args.old {
                return Err(AppError::new(
                    ErrorCode::InvalidArgument,
                    format!("owner.json names '{}' as the owner room", args.old),
                    "Post never rewrites the owner's signing config. Update owner.json to the new name first, then retry the rename.",
                )
                .input(args.old)
                .reason("owner.json names the old room"));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::io("inspect owner config", &owner_path, error)),
    }
    // The bridge interlock: on a bridged host, health.json must prove the
    // export guard is cleanly holding this host's names. Without that proof a
    // rename could let the bridge export this room's already-delivered
    // letters to another host that also carries the old name. `ok:false` is
    // not itself a refusal — a room_name_collision is what a rename fixes.
    let bridged = crate::bridge_topology::load_config(context)
        .map_err(|reason| {
            AppError::config(
                &crate::bridge_topology::bridge_dir(context).join("config.json"),
                reason,
            )
        })?
        .is_some();
    if bridged {
        crate::bridge_topology::export_guard_health(context, std::time::SystemTime::now())
            .map_err(|reason| {
                AppError::new(
                    ErrorCode::BridgeGuardUnavailable,
                    format!("cannot rename '{}': the bridge's export guard is not proven holding ({reason})", args.old),
                    "Check that the post bridge is running and healthy (it refreshes bridge/health.json every tick), then retry; nothing was written.",
                )
                .input(args.old.clone())
                .reason(reason)
            })?;
        let unheld = crate::bridge_topology::unheld_room_letters(context, &args.old).map_err(
            |reason| {
                AppError::new(
                    ErrorCode::BridgeGuardUnavailable,
                    format!("cannot rename '{}': cannot prove the bridge holds this room's letters ({reason})", args.old),
                    "Fix the unreadable store path, then retry; nothing was written.",
                )
                .input(args.old.clone())
                .reason(reason)
            },
        )?;
        if !unheld.is_empty() {
            let shown: Vec<String> = unheld.iter().take(8).cloned().collect();
            let more = unheld.len() - shown.len();
            return Err(AppError::new(
                ErrorCode::BridgeGuardUnavailable,
                format!(
                    "cannot rename '{}': {} letter(s) delivered to it have no export hold yet: {}{}",
                    args.old,
                    unheld.len(),
                    shown.join(", "),
                    if more > 0 { format!(" (and {more} more)") } else { String::new() }
                ),
                "The bridge stamps holds on its next full tick; retry after it runs. A letter still unheld after a full tick is one the export guard cannot protect: do not rename until it is resolved.",
            )
            .input(args.old.clone())
            .room(args.old.clone())
            .matches(shown)
            .reason("letters delivered to the room have no export hold"));
        }
    }
    let mut plan = RenamePlan {
        dir_move: if moved_before {
            None
        } else {
            match fs::symlink_metadata(&old_home) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    Some((old_home.clone(), new_home.clone()))
                }
                Ok(_) => {
                    return Err(AppError::config(
                    &old_home,
                    "the room's mailbox directory is a symlink or not a directory; repair or remove it before renaming",
                ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(AppError::io("inspect room directory", &old_home, error)),
            }
        },
        ..RenamePlan::default()
    };
    // The receipts are read where the mailbox is now and written where it
    // will be once the directory has moved.
    let home_now = if moved_before { &new_home } else { &old_home };
    plan_live_rewrites(
        context, &args.old, &args.new, home_now, &new_home, &mut plan,
    )?;

    let mut warnings = rename_warnings(context, &args.old, &args.new, bridged);
    if let Some(journal) = &journal {
        warnings.insert(0, resume_warning(journal));
    }
    warnings.extend(plan.warnings.clone());
    let mut rewritten: BTreeMap<String, usize> = BTreeMap::new();
    for write in &plan.writes {
        *rewritten.entry(write.store.to_owned()).or_default() += 1;
    }
    let output = RoomsRenameOutput {
        ok: true,
        old: args.old.clone(),
        new: args.new.clone(),
        path: stored_path.clone(),
        mailbox_moved: plan.dir_move.is_some() || moved_before,
        rewritten,
        warnings,
        resumed: resuming,
        dry_run: args.dry_run,
    };
    let result = CommandResult::json(&output, pretty)?;
    if args.dry_run {
        eprintln!("post: dry run: nothing was written");
        return Ok(result);
    }
    // The journal lands after every check and before the first store
    // change, so any crash from here on leaves a resumable record.
    if !resuming {
        write_rename_journal(context, &args.old, &args.new)?;
    }
    let committed = apply_rename(&plan, &old_home, &new_home, || {
        rooms.remove(&args.old);
        rooms.insert(args.new.clone(), stored_path);
        context.write_rooms(&rooms)
    });
    match committed {
        Ok(()) => {
            remove_rename_journal(context);
            Ok(result.registration_committed())
        }
        Err(failure) if failure.rolled_back && !resuming => {
            remove_rename_journal(context);
            Err(failure.error)
        }
        Err(failure) => Err(journal_kept_error(
            context,
            &args.old,
            &args.new,
            failure.error,
        )),
    }
}

fn write_rename_journal(context: &Context, old: &str, new: &str) -> AppResult<()> {
    let path = context.root.join(RENAME_JOURNAL_FILE);
    let journal = RenameJournal {
        v: 1,
        old: old.to_owned(),
        new: new.to_owned(),
        started_at: crate::mailbox::local_timestamp()?.1,
    };
    let mut bytes = serde_json::to_vec_pretty(&journal)
        .map_err(|error| AppError::io("serialize rename journal", &path, error))?;
    bytes.push(b'\n');
    crate::mailbox::atomic_replace(&path, &bytes)
        .map_err(|error| AppError::io("write rename journal", &path, error))
}

/// Remove the journal after a commit or a clean rollback. A failed removal
/// only warns: the store is consistent, and a standing journal for a
/// committed rename resumes to a no-op.
fn remove_rename_journal(context: &Context) {
    let path = context.root.join(RENAME_JOURNAL_FILE);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "post: warning: could not remove {}: {error}; `post doctor` will report it until it is gone",
            path.display()
        ),
    }
}

/// A failure that leaves the journal standing: the error keeps its cause and
/// the fix becomes the resume command. Shared by the fresh-rename and both
/// resume paths so every kept journal names how to finish.
fn journal_kept_error(context: &Context, old: &str, new: &str, mut error: AppError) -> AppError {
    error.suggested_fix = format!(
        "The rename stopped part-way and {} records it. Fix the cause above, then resume with `{}`.",
        context.root.join(RENAME_JOURNAL_FILE).display(),
        resume_command(old, new)
    );
    error
}

fn resume_warning(journal: &RenameJournal) -> String {
    format!(
        "resumed the rename of '{}' to '{}' started at {}",
        journal.old, journal.new, journal.started_at
    )
}

fn journal_standing_error(context: &Context, journal: &RenameJournal) -> AppError {
    let fix = resume_command(&journal.old, &journal.new);
    AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "an interrupted rename of '{}' to '{}' (started {}) is recorded in {}",
            journal.old,
            journal.new,
            journal.started_at,
            context.root.join(RENAME_JOURNAL_FILE).display()
        ),
        format!("Finish it first with `{fix}`, then retry this rename."),
    )
    .room(journal.old.clone())
    .exact_fix(fix)
    .reason("an interrupted rename must be resumed before another runs")
}

/// Resume refuses when `<root>/<old>` came back after the move (mail sent to
/// the old name before rooms.json committed). Merging is never automatic.
fn recreated_old_error(old: &str, new: &str, old_home: &Path) -> AppError {
    let mut files = Vec::new();
    list_files(old_home, old_home, &mut files);
    files.sort();
    let shown: Vec<String> = files.iter().take(20).cloned().collect();
    let more = files.len() - shown.len();
    AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "cannot resume the rename of '{old}' to '{new}': '{}' exists again and holds {} file(s): {}{}",
            old_home.display(),
            files.len(),
            shown.join(", "),
            if more > 0 { format!(" (and {more} more)") } else { String::new() }
        ),
        format!(
            "Rename never merges mailboxes. Move each listed file into the matching place under the '{new}' mailbox by hand (or remove it if it is not mail), remove '{}', then run `{}`.",
            old_home.display(),
            resume_command(old, new)
        ),
    )
    .room(old.to_owned())
    .path(old_home.display().to_string())
    .matches(shown)
    .reason("the old mailbox was recreated after the interrupted move")
}

fn list_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        out.push(format!("{} (unreadable)", dir.display()));
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => list_files(root, &path, out),
            _ => out.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            ),
        }
    }
}

/// Resume a rename whose rooms.json commit landed before the crash: re-plan
/// from NEW (every rewrite is idempotent), apply any rewrite still pending,
/// and remove the journal.
fn resume_committed_rename(
    context: &Context,
    args: &RoomsRenameArgs,
    rooms: &RoomMap,
    new_home: &Path,
    pretty: bool,
) -> AppResult<CommandResult> {
    let journal_path = context.root.join(RENAME_JOURNAL_FILE);
    let Some(stored_path) = rooms.get(&args.new).cloned() else {
        return Err(AppError::config(
            &journal_path,
            format!(
                "the journal records a rename of '{}' to '{}', but rooms.json registers neither; restore rooms.json, then resume",
                args.old, args.new
            ),
        ));
    };
    let journal = read_rename_journal(context)?.expect("resume runs with a journal");
    let mut plan = RenamePlan::default();
    plan_live_rewrites(context, &args.old, &args.new, new_home, new_home, &mut plan)?;
    let mut warnings = vec![resume_warning(&journal)];
    warnings.extend(plan.warnings.clone());
    let mut rewritten: BTreeMap<String, usize> = BTreeMap::new();
    for write in &plan.writes {
        *rewritten.entry(write.store.to_owned()).or_default() += 1;
    }
    let output = RoomsRenameOutput {
        ok: true,
        old: args.old.clone(),
        new: args.new.clone(),
        path: stored_path,
        mailbox_moved: new_home.is_dir(),
        rewritten,
        warnings,
        resumed: true,
        dry_run: args.dry_run,
    };
    let result = CommandResult::json(&output, pretty)?;
    if args.dry_run {
        eprintln!("post: dry run: nothing was written");
        return Ok(result);
    }
    apply_rename(&plan, new_home, new_home, || Ok(()))
        .map_err(|failure| journal_kept_error(context, &args.old, &args.new, failure.error))?;
    remove_rename_journal(context);
    Ok(result.registration_committed())
}

/// Everything `remote_duplicate_error` needs about one refused collision.
/// `fix_for` renders the runnable command for a chosen candidate; `retry`
/// names the command in prose when no candidate is derivable.
struct RemoteDuplicate<'a> {
    context: &'a Context,
    rooms: &'a RoomMap,
    placeholders: &'a BTreeMap<String, String>,
    existing_name: &'a str,
    requested: &'a str,
    host: &'a str,
    fix_for: &'a dyn Fn(&str) -> String,
    retry: &'a str,
}

/// The refusal `add` and `rename` share when the case-folded duplicate is a
/// remote placeholder: the owning host is machine-readable (`details.host`),
/// and the fix is the estate's `<name>-<suffix>` convention — learned from
/// this host's registrations, falling back to the bridge host id.
fn remote_duplicate_error(dup: RemoteDuplicate) -> AppError {
    // `set-path` refuses placeholders, so the local-duplicate hint can never
    // work here. The estate's naming rule gives the fix instead — this
    // checkout takes a `<name>-<suffix>` of its own.
    // A suggestion must survive every check `add` and `rename` run on a new
    // name, or the exact_fix runs and then refuses. A store that cannot be
    // read proves nothing, so the candidate is not offered.
    let rules = dup.context.load_rules(dup.rooms).ok();
    let is_free = |candidate: &str| {
        matches!(crate::lineage::load(dup.context, candidate), Ok(None))
            && rules
                .as_ref()
                .is_some_and(|rules| !rules.blocked.iter().any(|rule| rule.targets(candidate)))
    };
    let candidate = suffixed_room_candidate(
        dup.rooms,
        dup.placeholders,
        bridge_host_id(dup.context).as_deref(),
        dup.requested,
        &is_free,
    );
    let mut error = AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "room '{}' is already registered as '{}', a remote placeholder owned by host '{}': a checkout on this machine needs its own name",
            dup.requested, dup.existing_name, dup.host
        ),
        match &candidate {
            Some(candidate) => {
                format!("This checkout needs its own name; run `{}`.", (dup.fix_for)(candidate))
            }
            None => format!(
                "This checkout needs its own name (the estate convention is `<name>-<host-suffix>`), but no suffixed candidate is free or derivable here; pick one and retry {}.",
                dup.retry
            ),
        },
    )
    .input(dup.requested.to_owned())
    .room(dup.existing_name.to_owned())
    .host(dup.host.to_owned())
    .reason("room name is a remote placeholder owned by another host");
    if let Some(candidate) = candidate {
        error = error.exact_fix((dup.fix_for)(&candidate));
    }
    error
}

/// One file the rename rewrites, with its original bytes for rollback.
struct RenameWrite {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
    store: &'static str,
}

/// The full set of planned changes, computed before anything is written.
#[derive(Default)]
struct RenamePlan {
    dir_move: Option<(PathBuf, PathBuf)>,
    writes: Vec<RenameWrite>,
    /// Non-fatal findings folded into the receipt (skipped malformed records).
    warnings: Vec<String>,
    /// Participant cursor locks held from before a planned `cursors.json` is
    /// read until the rename commits or rolls back, so no concurrent cursor
    /// writer can restore a pre-rename snapshot.
    cursor_locks: Vec<fs::File>,
}

/// True when `bytes` cannot be proven free of a reference to `old`: the name
/// appears as a whole JSON string or inside a `workspace:` cursor key.
fn json_bytes_may_name_room(bytes: &[u8], old: &str) -> bool {
    let as_string = format!("\"{old}\"");
    let as_cursor_key = format!("\"workspace:{old}\"");
    let Ok(text) = std::str::from_utf8(bytes) else {
        // Non-UTF-8 cannot be JSON at all; it also cannot carry a JSON
        // string naming the room, so it needs no reference.
        return false;
    };
    text.contains(&as_string) || text.contains(&as_cursor_key)
}

/// Load a JSON state file for the rewrite plan. A malformed file that might
/// still name the old room refuses the rename — live references must never
/// be silently left behind; a malformed file that provably cannot name it is
/// skipped with a warning, matching the readers' own tolerate-corruption
/// posture.
fn plan_json_write(
    path: PathBuf,
    old: &str,
    store: &'static str,
    plan: &mut RenamePlan,
    mutate: impl FnOnce(&mut serde_json::Value) -> AppResult<()>,
) -> AppResult<()> {
    let original = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::io("read state for rename", &path, error)),
    };
    let mut value: serde_json::Value = match serde_json::from_slice(&original) {
        Ok(value) => value,
        Err(error) => {
            if json_bytes_may_name_room(&original, old) {
                return Err(AppError::config(
                    &path,
                    format!("cannot prove a malformed {store} file is free of '{old}' references: {error}"),
                ));
            }
            plan.warnings.push(format!(
                "skipped malformed {} at {}: {error}",
                store,
                path.display()
            ));
            return Ok(());
        }
    };
    // Compare documents, not bytes: a file that never named the room must not
    // be rewritten (or counted) just because its serialization style differs.
    let unchanged = value.clone();
    mutate(&mut value)?;
    if value == unchanged {
        return Ok(());
    }
    let mut updated = serde_json::to_vec_pretty(&value)
        .map_err(|error| AppError::io("serialize renamed state", &path, error))?;
    updated.push(b'\n');
    plan.writes.push(RenameWrite {
        path,
        original,
        updated,
        store,
    });
    Ok(())
}

/// Move a JSON object's `old` key to `new`. When `new` already exists the
/// renamed room's record wins — a stray same-named entry predates the rename
/// and does not describe this room — and the overwrite is reported: returns
/// `Some(true)` when an existing `new` key was replaced, `Some(false)` for a
/// plain move, and None when `old` was absent.
fn move_json_key(
    map: &mut serde_json::Map<String, serde_json::Value>,
    old: &str,
    new: &str,
) -> Option<bool> {
    let value = map.remove(old)?;
    Some(map.insert(new.to_owned(), value).is_some())
}

/// The receipt warning for an existing `new` key a rename replaced.
fn overwrite_warning(store: &str, path: &Path, new: &str) -> String {
    format!(
        "{store} at {} already had an entry for '{new}'; the renamed room's entry replaced it",
        path.display()
    )
}

/// Union the `seen` id arrays of two cursor entries, sorted and deduplicated.
fn union_seen_values(
    into: &mut serde_json::Value,
    from: serde_json::Value,
    path: &Path,
) -> AppResult<()> {
    let seen_of = |value: &serde_json::Value| -> AppResult<Vec<String>> {
        let Some(array) = value.get("seen").and_then(|seen| seen.as_array()) else {
            return Err(AppError::config(
                path,
                "participant cursors.json holds a mail entry without a 'seen' array",
            ));
        };
        array
            .iter()
            .map(|id| {
                id.as_str().map(str::to_owned).ok_or_else(|| {
                    AppError::config(
                        path,
                        "participant cursors.json 'seen' holds a non-string id",
                    )
                })
            })
            .collect()
    };
    let mut ids = seen_of(into)?;
    ids.extend(seen_of(&from)?);
    ids.sort();
    ids.dedup();
    into["seen"] =
        serde_json::Value::Array(ids.into_iter().map(serde_json::Value::String).collect());
    Ok(())
}

/// Collect every live-state file that names the room, in rewrite order.
/// Each entry pairs the file's original bytes (restored on rollback) with
/// the rewritten document.
fn plan_live_rewrites(
    context: &Context,
    old: &str,
    new: &str,
    home_now: &Path,
    new_home: &Path,
    plan: &mut RenamePlan,
) -> AppResult<()> {
    // <room>/routing/<id>.json: every receipt binds the address it routed
    // for, and `routing::validate_receipt` refuses a receipt whose address
    // is not the reader's. The receipt is live routing state, so it follows
    // the room.
    plan_routing_receipts(old, new, home_now, new_home, plan)?;

    // participants/<id>/participant.json: the `workspace` field. cursors.json:
    // mail seen-keys `workspace:<old>` (a `workspace:<new>` key that already
    // exists merges, never loses read state).
    let participants_root = context.root.join(crate::participant::PARTICIPANTS_DIR);
    match fs::read_dir(&participants_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|error| {
                    AppError::io("read participant entry", &participants_root, error)
                })?;
                if !entry
                    .file_type()
                    .map_err(|error| {
                        AppError::io("inspect participant entry", &entry.path(), error)
                    })?
                    .is_dir()
                {
                    continue;
                }
                // by-session holds the session index, not participant state.
                if entry.file_name() == "by-session" {
                    continue;
                }
                let dir = entry.path();
                let old_for_record = old.to_owned();
                let new_for_record = new.to_owned();
                plan_json_write(
                    dir.join("participant.json"),
                    old,
                    "participants",
                    plan,
                    move |value| {
                        if value.get("workspace").and_then(|w| w.as_str())
                            == Some(old_for_record.as_str())
                        {
                            value["workspace"] = serde_json::Value::String(new_for_record.clone());
                        }
                        Ok(())
                    },
                )?;
                let cursor_path = dir.join(crate::cursor_state::CURSORS_FILE);
                // Lock before reading: the planned bytes must still be the
                // file's bytes when the rewrite lands.
                let cursor_lock = match fs::symlink_metadata(&cursor_path) {
                    Ok(_) => Some(crate::cursor_state::lock_participant_cursors(&dir)?),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(AppError::io(
                            "inspect participant cursors",
                            &cursor_path,
                            error,
                        ))
                    }
                };
                let writes_before = plan.writes.len();
                let old_key = format!("workspace:{old}");
                let new_key = format!("workspace:{new}");
                plan_json_write(
                    cursor_path.clone(),
                    old,
                    "participant_cursors",
                    plan,
                    move |value| {
                        let Some(mail) =
                            value.get_mut("mail").and_then(|mail| mail.as_object_mut())
                        else {
                            return Ok(());
                        };
                        if let Some(moved) = mail.remove(&old_key) {
                            match mail.get_mut(&new_key) {
                                Some(existing) => {
                                    union_seen_values(existing, moved, &cursor_path)?;
                                }
                                None => {
                                    mail.insert(new_key, moved);
                                }
                            }
                        }
                        Ok(())
                    },
                )?;
                if let Some(lock) = cursor_lock {
                    if plan.writes.len() > writes_before {
                        plan.cursor_locks.push(lock);
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::io("list participants", &participants_root, error)),
    }

    // channels/<channel>/members.json: the legacy workspace-keyed member map.
    let channels_root = context.root.join(crate::channel::CHANNELS_DIR);
    match fs::read_dir(&channels_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry
                    .map_err(|error| AppError::io("read channel entry", &channels_root, error))?;
                if !entry
                    .file_type()
                    .map_err(|error| AppError::io("inspect channel entry", &entry.path(), error))?
                    .is_dir()
                {
                    continue;
                }
                let members_path = entry.path().join("members.json");
                let mut overwrote = false;
                plan_json_write(
                    members_path.clone(),
                    old,
                    "channel_members",
                    plan,
                    |value| {
                        if let Some(map) = value.as_object_mut() {
                            overwrote = move_json_key(map, old, new) == Some(true);
                        }
                        Ok(())
                    },
                )?;
                if overwrote {
                    plan.warnings
                        .push(overwrite_warning("channel_members", &members_path, new));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::io("list channels", &channels_root, error)),
    }

    // profiles.json: legacy bare (workspace-keyed) entries only; typed
    // `participant:<id>` keys never name a room.
    let profiles_path = context.root.join(crate::profile::PROFILES_FILE);
    let mut overwrote = false;
    plan_json_write(profiles_path.clone(), old, "profiles", plan, |value| {
        if let Some(map) = value.as_object_mut() {
            if !old.starts_with(crate::profile::PARTICIPANT_KEY_PREFIX) {
                overwrote = move_json_key(map, old, new) == Some(true);
            }
        }
        Ok(())
    })?;
    if overwrote {
        plan.warnings
            .push(overwrite_warning("profiles", &profiles_path, new));
    }
    Ok(())
}

/// Plan the rewrite of every routing receipt in the room that binds
/// `workspace:<old>`: only `address.name` changes. Receipts are read from
/// `home_now/routing` and written to `new_home/routing` (the directory moves
/// before any rewrite lands; on resume the two are the same). A receipt is
/// re-encoded exactly as `routing::publish_receipt` writes one, so the result
/// is byte-identical to the receipt routing would have written for `new`. A
/// receipt for any other address is left alone; a malformed one follows the
/// `plan_json_write` policy.
fn plan_routing_receipts(
    old: &str,
    new: &str,
    home_now: &Path,
    new_home: &Path,
    plan: &mut RenamePlan,
) -> AppResult<()> {
    let routing_now = home_now.join("routing");
    let entries = match fs::read_dir(&routing_now) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::io("list routing receipts", &routing_now, error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read routing entry", &routing_now, error))?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue };
        if !text.ends_with(".json") || text.starts_with('.') {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| AppError::io("inspect routing entry", &entry.path(), error))?
            .is_file()
        {
            continue;
        }
        names.push(text.to_owned());
    }
    names.sort();
    for name in names {
        let source = routing_now.join(&name);
        let original = fs::read(&source)
            .map_err(|error| AppError::io("read routing receipt for rename", &source, error))?;
        let mut receipt: crate::cursor_state::routing::Receipt = match serde_json::from_slice(
            &original,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                if json_bytes_may_name_room(&original, old) {
                    return Err(AppError::config(
                        &source,
                        format!("cannot prove a malformed routing receipt is free of '{old}' references: {error}"),
                    ));
                }
                plan.warnings.push(format!(
                    "skipped malformed routing_receipts at {}: {error}",
                    source.display()
                ));
                continue;
            }
        };
        if receipt.address.kind != crate::participant::AddressKind::Workspace
            || receipt.address.name != old
        {
            continue;
        }
        receipt.address.name = new.to_owned();
        let target = new_home.join("routing").join(&name);
        let mut updated = serde_json::to_vec_pretty(&receipt)
            .map_err(|error| AppError::io("serialize renamed routing receipt", &target, error))?;
        updated.push(b'\n');
        plan.writes.push(RenameWrite {
            path: target,
            original,
            updated,
            store: "routing_receipts",
        });
    }
    Ok(())
}

/// The receipt warnings a rename always or conditionally carries.
fn rename_warnings(context: &Context, old: &str, new: &str, bridged: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    // The heartbeat moves with the directory, so a live watch on the old name
    // still stamps the moved file — but it answers for nobody. Anything that
    // armed on the old name (a watcher, a Monitor doorbell) must be re-armed.
    if crate::presence::read_presence(context, old)
        .map(|presence| presence.live_watch)
        .unwrap_or(false)
    {
        warnings.push(format!(
            "a watcher or doorbell armed on '{old}' was live recently; re-arm it on '{new}'"
        ));
    }
    warnings.push(format!(
        "post does not update names outside its store: doorbells, supervisor config, Porch config, and CLAUDE.md files still name '{old}'"
    ));
    if bridged {
        warnings.push(format!(
            "the bridge publishes '{new}' on its next tick; another host's '{old}', if any, becomes that host's alone"
        ));
    }
    warnings
}

/// A failed apply: the first error, and whether the rollback restored every
/// file and moved the mailbox back.
struct RenameFailure {
    error: AppError,
    rolled_back: bool,
}

/// Apply a planned rename, rolling back on the first failure: move the
/// mailbox, rewrite live state, then run `commit` (the rooms.json write).
/// Any failure, the commit's included, restores rewritten files to their
/// original bytes and moves the mailbox back. `commit` must leave its
/// target untouched when it fails (`atomic_replace` does).
fn apply_rename(
    plan: &RenamePlan,
    old_home: &Path,
    new_home: &Path,
    commit: impl FnOnce() -> AppResult<()>,
) -> Result<(), RenameFailure> {
    let mut applied = 0usize;
    let mut moved = false;
    let outcome = (|| -> AppResult<()> {
        if plan.dir_move.is_some() {
            fs::rename(old_home, new_home)
                .map_err(|error| AppError::io("move room mailbox", new_home, error))?;
            moved = true;
        }
        for write in &plan.writes {
            crate::mailbox::atomic_replace(&write.path, &write.updated).map_err(|error| {
                AppError::io("rewrite live state for rename", &write.path, error)
            })?;
            applied += 1;
        }
        commit()
    })();
    let Err(error) = outcome else {
        return Ok(());
    };
    let mut rolled_back = true;
    for write in plan.writes[..applied].iter().rev() {
        if let Err(restore) = crate::mailbox::atomic_replace(&write.path, &write.original) {
            rolled_back = false;
            eprintln!(
                "post: warning: rollback could not restore {}: {restore}",
                write.path.display()
            );
        }
    }
    if moved {
        // Something recreated `<old>` after the move (an out-of-repo writer):
        // moving back would merge or fail, so the rollback is incomplete and
        // the journal must stay for the resume's recreated-old refusal.
        if fs::symlink_metadata(old_home).is_ok() {
            rolled_back = false;
            eprintln!(
                "post: warning: rollback could not move the mailbox back: {} exists again; the mail stays at {}",
                old_home.display(),
                new_home.display()
            );
        } else if let Err(restore) = fs::rename(new_home, old_home) {
            rolled_back = false;
            eprintln!(
                "post: warning: rollback could not move the mailbox back to {}: {restore}",
                old_home.display()
            );
        }
    }
    Err(RenameFailure { error, rolled_back })
}

/// Expand and canonicalize a room path argument, requiring an existing
/// directory. Shared by `add` and `set-path` so both accept exactly the same
/// paths. Returns the expanded and canonical forms.
fn validate_workspace_path(context: &Context, raw: &str) -> AppResult<(PathBuf, PathBuf)> {
    let expanded = context.expand_room_path(raw).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room path '{raw}' is invalid: {reason}"),
            "Pass an existing directory using an absolute path or a path starting with '~/'.",
        )
        .input(raw.to_owned())
        .reason(reason)
    })?;
    let canonical = fs::canonicalize(&expanded).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AppError::new(
                ErrorCode::InvalidArgument,
                format!("room path '{}' does not exist", expanded.display()),
                "Create the workspace directory, then retry the same command.",
            )
            .input(raw.to_owned())
            .reason("path does not exist")
        } else {
            AppError::io("inspect room path", &expanded, error)
        }
    })?;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| AppError::io("inspect room path", &canonical, error))?;
    if !metadata.is_dir() {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!("room path '{}' is not a directory", expanded.display()),
            "Pass an existing workspace directory.",
        )
        .input(raw)
        .reason("path is not a directory"));
    }

    Ok((expanded, canonical))
}

/// Refuse a path another room already owns (`skip` names the room being
/// re-pointed, whose own current path is not a conflict). Returns warnings
/// for registered rooms whose paths could not be canonicalized.
fn ensure_workspace_unclaimed(
    context: &Context,
    rooms: &RoomMap,
    expanded: &Path,
    canonical: &Path,
    raw: &str,
    skip: Option<&str>,
) -> AppResult<Vec<String>> {
    let normalized_canonical = normalize_path(canonical);
    let normalized_expanded = normalize_path(expanded);
    let mut warnings = Vec::new();
    for (room, room_path) in rooms {
        if Some(room.as_str()) == skip {
            continue;
        }
        let existing = context
            .expand_room_path(room_path)
            .map_err(|reason| AppError::config(&context.root.join("rooms.json"), reason))?;
        let duplicate = match fs::canonicalize(&existing) {
            Ok(existing) => existing == canonical,
            Err(error) => {
                let normalized_existing = normalize_path(&existing);
                let duplicate = normalized_existing == normalized_canonical
                    || normalized_existing == normalized_expanded;
                if !duplicate {
                    warnings.push(format!(
                        "post: warning: registered room {room:?} at {existing:?} could not be canonicalized ({:?}); duplicate-workspace checks for it are limited to normalized path strings",
                        error.kind()
                    ));
                }
                duplicate
            }
        };
        if duplicate {
            return Err(AppError::new(
                ErrorCode::DuplicateWorkspace,
                format!(
                    "workspace '{}' is already registered as room '{room}'",
                    canonical.display()
                ),
                "Use the existing room name; workspace aliases are not allowed.",
            )
            .input(raw)
            .room(room)
            .registered_path(canonical.display().to_string())
            .reason("workspace path is already registered"));
        }
    }

    Ok(warnings)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if fs::symlink_metadata(&normalized)
                    .is_ok_and(|metadata| !metadata.file_type().is_symlink())
                {
                    normalized.pop();
                } else {
                    normalized.push(component.as_os_str());
                }
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn render(rooms: &RoomMap, rules: &RulesConfig, pretty: bool) -> AppResult<CommandResult> {
    let output_rooms: Vec<_> = rooms
        .iter()
        .map(|(name, path)| {
            let blocked = rules
                .blocked
                .iter()
                .filter(|rule| rule.targets(name))
                .cloned()
                .collect();
            RoomOutput {
                name: name.clone(),
                path: path.clone(),
                blocked,
            }
        })
        .collect();
    let count = output_rooms.len();
    let output = RoomsOutput {
        ok: true,
        rooms: output_rooms,
        count,
    };
    CommandResult::json(&output, pretty)
}

/// Every remote placeholder in the registry, name to owning host — the
/// rooms.json entries whose stored path lands under `<root>/remote/<host>/`.
fn placeholder_hosts(context: &Context, rooms: &RoomMap) -> BTreeMap<String, String> {
    rooms
        .iter()
        .filter_map(|(name, stored)| {
            crate::output::stored_path_remote_host_of(context, stored)
                .map(|host| (name.clone(), host))
        })
        .collect()
}

/// This host's bridge id from `bridge/config.json`, or None on an unbridged
/// host or a config that cannot be trusted.
fn bridge_host_id(context: &Context) -> Option<String> {
    crate::bridge_topology::load_config(context)
        .ok()
        .flatten()
        .map(|config| config.host)
}

/// The estate's `<name>-<host-suffix>` candidate for a refused name: the
/// first of the learned suffixes (in rank order), then the bridge host id,
/// whose candidate is a valid room name, is not taken under ASCII case
/// folding, and passes `is_free` (no lineage of that name, no blocked route
/// to it). None when no candidate survives.
fn suffixed_room_candidate(
    rooms: &RoomMap,
    placeholders: &BTreeMap<String, String>,
    bridge_host: Option<&str>,
    name: &str,
    is_free: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let mut suffixes = learned_host_suffixes(rooms, placeholders);
    if let Some(host) = bridge_host {
        if !suffixes.iter().any(|suffix| suffix == host) {
            suffixes.push(host.to_owned());
        }
    }
    suffixes
        .into_iter()
        .map(|suffix| format!("{name}-{suffix}"))
        .find(|candidate| {
            validate_new_room_name(candidate).is_ok()
                && !rooms
                    .keys()
                    .any(|taken| taken.eq_ignore_ascii_case(candidate))
                && is_free(candidate)
        })
}

/// The estate's `<base>-<host-suffix>` naming rule, learned from this host's
/// own registrations: a local (non-placeholder) room named `<base>-<s>`
/// votes for `s` only when `<base>` is a remote placeholder AND the room's
/// own registered directory is named `<base>` (ASCII case-insensitive) — a
/// checkout of the placeholder's repo under this host's suffix, like the
/// devbox's `hq-devbox` at `.../hq`. A room whose directory carries the
/// full hyphenated name (the Mac's `cos-crons` at `.../cos-crons`) is its
/// own project, not a suffixed checkout, and does not vote. The most
/// frequent suffix wins; ties go to the lexicographically smallest. `None`
/// when no local room votes — the caller then falls back to the bridge host
/// id.
#[cfg(test)]
fn learned_host_suffix(rooms: &RoomMap, placeholders: &BTreeMap<String, String>) -> Option<String> {
    learned_host_suffixes(rooms, placeholders)
        .into_iter()
        .next()
}

/// Every learned suffix, best first (most votes, ties to the
/// lexicographically smallest); see `learned_host_suffix` for the vote.
fn learned_host_suffixes(rooms: &RoomMap, placeholders: &BTreeMap<String, String>) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (name, stored_path) in rooms {
        if placeholders.contains_key(name) {
            continue;
        }
        let Some(dir_name) = Path::new(stored_path).file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        for (index, _) in name.match_indices('-') {
            let (base, suffix) = (&name[..index], &name[index + 1..]);
            if !suffix.is_empty()
                && placeholders.contains_key(base)
                && dir_name.eq_ignore_ascii_case(base)
            {
                *counts.entry(suffix.to_owned()).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|(a_suffix, a_count), (b_suffix, b_count)| {
        b_count.cmp(a_count).then(a_suffix.cmp(b_suffix))
    });
    ranked.into_iter().map(|(suffix, _)| suffix).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};

    /// H1: when `<old>` exists again at rollback time (an out-of-repo writer
    /// recreated it after the move), the mailbox cannot move back. The
    /// rollback must report itself incomplete, so the caller keeps the
    /// journal, and must leave both directories untouched rather than merge.
    #[test]
    fn rollback_that_cannot_move_the_mailbox_back_is_not_clean() {
        let root = test_root("rename-rollback");
        let old_home = root.join("alpha");
        let new_home = root.join("alpha2");
        fs::create_dir_all(old_home.join("inbox")).expect("old inbox");
        fs::write(old_home.join("inbox/m1.mail"), "moved mail").expect("mail");
        let plan = RenamePlan {
            dir_move: Some((old_home.clone(), new_home.clone())),
            ..RenamePlan::default()
        };
        let failure = apply_rename(&plan, &old_home, &new_home, || {
            fs::create_dir_all(old_home.join("inbox")).expect("recreate old");
            fs::write(old_home.join("inbox/late.mail"), "late").expect("late mail");
            Err(AppError::config(
                &root.join("rooms.json"),
                "forced commit failure",
            ))
        })
        .expect_err("the commit failed");
        assert!(
            !failure.rolled_back,
            "a skipped move-back is a failed rollback"
        );
        assert_eq!(
            fs::read(new_home.join("inbox/m1.mail")).unwrap(),
            b"moved mail"
        );
        assert_eq!(fs::read(old_home.join("inbox/late.mail")).unwrap(), b"late");
        assert!(!old_home.join("inbox/m1.mail").exists(), "never merged");

        // Control: with nothing recreated, the same failure rolls back clean.
        fs::remove_dir_all(&old_home).expect("clear");
        fs::rename(&new_home, &old_home).expect("reset");
        let failure = apply_rename(&plan, &old_home, &new_home, || {
            Err(AppError::config(
                &root.join("rooms.json"),
                "forced commit failure",
            ))
        })
        .expect_err("the commit failed");
        assert!(failure.rolled_back);
        assert!(old_home.join("inbox/m1.mail").is_file() && !new_home.exists());
        trash_test_root(&root);
    }

    /// Rooms at `/workspaces/<name>`: a directory named for the whole room
    /// name, so no suffix vote comes from these.
    fn room_map(names: &[&str]) -> RoomMap {
        names
            .iter()
            .map(|name| ((*name).to_owned(), format!("/workspaces/{name}")))
            .collect()
    }

    /// Rooms at explicit directories: `(name, dir)`.
    fn room_map_at(entries: &[(&str, &str)]) -> RoomMap {
        entries
            .iter()
            .map(|(name, dir)| ((*name).to_owned(), (*dir).to_owned()))
            .collect()
    }

    fn placeholder_map(names: &[&str]) -> BTreeMap<String, String> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), "mac".to_owned()))
            .collect()
    }

    #[test]
    fn learned_suffix_is_the_majority_marker_on_remote_bases() {
        // The devbox shape: each local checkout carries the remote base plus
        // this host's suffix; one oddball must not outvote the convention.
        let rooms = room_map_at(&[
            ("hq-devbox", "/home/trey/Code/hq"),
            ("cos-devbox", "/home/trey/Code/cos"),
            ("fable-devbox", "~/Code/fable/"),
            ("hq-mac", "/home/trey/Code/hq"),
            ("plain", "/home/trey/Code/plain"),
        ]);
        let placeholders = placeholder_map(&["hq", "cos", "fable"]);
        assert_eq!(
            learned_host_suffix(&rooms, &placeholders).as_deref(),
            Some("devbox")
        );
    }

    #[test]
    fn learned_suffix_breaks_ties_on_the_lexicographically_smallest() {
        let rooms = room_map_at(&[("hq-zed", "/c/hq"), ("cos-aaa", "/c/cos")]);
        let placeholders = placeholder_map(&["hq", "cos"]);
        assert_eq!(
            learned_host_suffix(&rooms, &placeholders).as_deref(),
            Some("aaa")
        );
    }

    #[test]
    fn learned_suffix_ignores_placeholder_names_and_placeholderless_bases() {
        // `hq-mac` is itself a placeholder: it never votes. `other-mac` has a
        // base that is not a placeholder, so it does not vote either.
        let rooms = room_map_at(&[("hq-mac", "/c/hq"), ("other-mac", "/c/other")]);
        let placeholders = placeholder_map(&["hq", "hq-mac"]);
        assert_eq!(learned_host_suffix(&rooms, &placeholders), None);
    }

    #[test]
    fn learned_suffix_needs_the_directory_to_be_named_for_the_base() {
        // The live Mac shape: `cos-crons` is its own project at
        // `.../cos-crons`, and `cos` is a remote placeholder. It must not
        // teach "crons"; with no vote the bridge host id supplies the suffix.
        let rooms = room_map_at(&[
            ("cos", "/root/remote/devbox/cos"),
            ("cos-crons", "/Users/trey/Code/cos-crons"),
        ]);
        let placeholders = placeholder_map(&["cos"]);
        assert_eq!(learned_host_suffix(&rooms, &placeholders), None);
        assert_eq!(
            suffixed_room_candidate(&rooms, &placeholders, Some("mac"), "atlasos", &|_| true)
                .as_deref(),
            Some("atlasos-mac")
        );
        // The devbox shape: `hq-devbox` lives at `.../hq` (compared
        // case-insensitively), so it votes "devbox".
        let rooms = room_map_at(&[
            ("hq", "/root/remote/mac/hq"),
            ("hq-devbox", "/home/trey/Code/HQ"),
        ]);
        let placeholders = placeholder_map(&["hq"]);
        assert_eq!(
            learned_host_suffix(&rooms, &placeholders).as_deref(),
            Some("devbox")
        );
    }

    #[test]
    fn learned_suffix_reads_every_hyphen_split() {
        // The directory decides which split is a checkout: at `.../a` the
        // base is `a` (suffix `b-c`); at `.../a-b` the base is `a-b`.
        let placeholders = placeholder_map(&["a", "a-b"]);
        let rooms = room_map_at(&[("a-b-c", "/c/a")]);
        assert_eq!(
            learned_host_suffix(&rooms, &placeholders).as_deref(),
            Some("b-c")
        );
        let rooms = room_map_at(&[("a-b-c", "/c/a-b")]);
        assert_eq!(
            learned_host_suffix(&rooms, &placeholders).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn suffixed_candidate_falls_back_to_the_bridge_host() {
        let rooms = room_map(&["hq"]);
        let placeholders = placeholder_map(&["hq"]);
        assert_eq!(
            suffixed_room_candidate(&rooms, &placeholders, Some("trey"), "hq", &|_| true)
                .as_deref(),
            Some("hq-trey")
        );
        // No learned suffix and no bridge config: nothing is derivable.
        assert_eq!(
            suffixed_room_candidate(&rooms, &placeholders, None, "hq", &|_| true),
            None
        );
    }

    #[test]
    fn suffixed_candidate_skips_a_name_that_is_not_free_for_the_next() {
        // The learned "devbox" candidate collides (a lineage, say); the
        // bridge host id supplies the next one.
        let rooms = room_map_at(&[("hq", "/r/hq"), ("cos-devbox", "/c/cos")]);
        let placeholders = placeholder_map(&["hq", "cos"]);
        assert_eq!(
            suffixed_room_candidate(&rooms, &placeholders, Some("trey"), "hq", &|c| {
                c != "hq-devbox"
            })
            .as_deref(),
            Some("hq-trey")
        );
        // Nothing free: no suggestion.
        assert_eq!(
            suffixed_room_candidate(&rooms, &placeholders, Some("trey"), "hq", &|_| false),
            None
        );
    }

    #[test]
    fn suffixed_candidate_is_none_when_taken_or_invalid() {
        // Learned "mac" collides with the registered `hq-mac`.
        let rooms = room_map_at(&[("hq", "/r/hq"), ("hq-mac", "/c/hq")]);
        let placeholders = placeholder_map(&["hq"]);
        assert_eq!(
            suffixed_room_candidate(&rooms, &placeholders, None, "hq", &|_| true),
            None
        );
        // A candidate outside the room-name grammar is never suggested
        // (a learned suffix cannot produce this; the check is defensive).
        let rooms = room_map(&["hq", "hq-mac"]);
        assert_eq!(
            suffixed_room_candidate(&rooms, &placeholders, Some("a:b"), "hq", &|_| true),
            None
        );
    }
}
