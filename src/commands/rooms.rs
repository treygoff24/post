use crate::cli::{RoomsAddArgs, RoomsArgs, RoomsCommand};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{validate_room_name, Context};
use crate::model::{RoomMap, RulesConfig};
use crate::output::{RoomOutput, RoomsOutput};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) fn run(context: &Context, args: RoomsArgs, pretty: bool) -> AppResult<CommandResult> {
    match args.command {
        Some(RoomsCommand::Add(args)) => add(context, args, pretty),
        None => list(context, pretty),
    }
}

fn list(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    render(&rooms, &rules, pretty)
}

fn add(context: &Context, args: RoomsAddArgs, pretty: bool) -> AppResult<CommandResult> {
    validate_room_name(&args.name).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room name '{}' is invalid: {reason}", args.name),
            "Pass a single path-safe room name without '/' or '\\'.",
        )
        .input(args.name.clone())
        .reason(reason)
    })?;

    let _lock = context.lock_rooms()?;
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
            "Choose a new room name; edit rooms.json by hand if the existing path is wrong.",
        )
        .input(args.name)
        .room(existing_name)
        .reason("duplicate room name under ASCII case folding"));
    }

    let expanded = context.expand_room_path(&args.path).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room path '{}' is invalid: {reason}", args.path),
            "Pass an existing directory using an absolute path or a path starting with '~/'.",
        )
        .input(args.path.clone())
        .reason(reason)
    })?;
    let canonical = fs::canonicalize(&expanded).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AppError::new(
                ErrorCode::InvalidArgument,
                format!("room path '{}' does not exist", expanded.display()),
                "Create the workspace directory, then retry the same command.",
            )
            .input(args.path.clone())
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
        .input(args.path)
        .reason("path is not a directory"));
    }

    let normalized_canonical = normalize_path(&canonical);
    let normalized_expanded = normalize_path(&expanded);
    let mut warnings = Vec::new();
    for (room, room_path) in &rooms {
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
            .input(args.path)
            .room(room)
            .registered_path(canonical.display().to_string())
            .reason("workspace path is already registered"));
        }
    }

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
