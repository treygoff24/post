use crate::cli::{RoomsAddArgs, RoomsArgs, RoomsCommand, RoomsSetPathArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{validate_new_room_name, Context};
use crate::model::{RoomMap, RulesConfig};
use crate::output::{RoomOutput, RoomsOutput, RoomsSetPathOutput};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) fn run(context: &Context, args: RoomsArgs, pretty: bool) -> AppResult<CommandResult> {
    match args.command {
        Some(RoomsCommand::Add(args)) => add(context, args, pretty),
        Some(RoomsCommand::SetPath(args)) => set_path(context, args, pretty),
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
