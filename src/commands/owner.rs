use crate::cli::{OwnerArgs, OwnerCommand, OwnerInitArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{load_owner, Context, OwnerFile, OwnerResolution, ResolvedOwner};
use serde::Serialize;
use std::fs;
use std::path::Path;

const GUIDE: &str =
    "porch generates the signing key pair and authors allowed_signers; post never generates keys — post verifies, porch signs.";

pub(super) fn run(context: &Context, args: OwnerArgs, pretty: bool) -> AppResult<CommandResult> {
    match args.command {
        Some(OwnerCommand::Init(args)) => init(context, args, pretty),
        // A bare `post owner` and `post owner show` both print the resolved owner.
        Some(OwnerCommand::Show) | None => show(context, pretty),
    }
}

/// `post owner init`: declare the signed owner. Create-only and atomic: an
/// existing identical owner.json is an idempotent success, an existing
/// different or malformed one is refused, a symlink at the path is refused,
/// and a destination appearing between the temp write and the hard-link
/// commit routes to the same compare branch (it is never replaced).
fn init(context: &Context, args: OwnerInitArgs, pretty: bool) -> AppResult<CommandResult> {
    let path = context.owner_json_path();
    let file = OwnerFile {
        room: args.room,
        sidecar_dir: args.sidecar_dir,
        allowed_signers: args.allowed_signers,
        principal: args.principal,
        namespace: args.namespace,
        marker: args.marker,
        label: args.label,
    };
    // Every supplied flag passes the same validation as a hand-written
    // owner.json: init can never write a file its own loader would refuse.
    crate::mailbox::validate_owner_values(&path, &file)?;

    // The rooms lock doubles as the owner-config lock: registration and the
    // derived sidecar path are re-validated INSIDE the lock, so `owner init`
    // cannot race a concurrent room registration/rename into a mismatched
    // derived sidecar.
    let _lock = context.lock_rooms()?;
    let rooms = context.load_rooms()?;
    let would_be = crate::mailbox::resolve_owner_file(context, &file, &rooms)?;

    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AppError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "owner.json at '{}' is a symlink; refusing to follow or replace a symlinked trust anchor",
                    path.display()
                ),
                "Move the symlink aside, then run `post owner init` again.",
            )
            .path(path.display().to_string()));
        }
        Ok(_) => return compare_existing(context, &path, &would_be, pretty),
        Err(error) => return Err(AppError::io("inspect owner config", &path, error)),
    }

    let mut bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| AppError::io("serialize owner config", &path, error))?;
    bytes.push(b'\n');
    // Create-only commit via the no-replace primitive: `create_new` temp,
    // write+sync, then hard_link to the destination. A destination created
    // between the precheck above and this commit surfaces as AlreadyExists
    // and routes to the compare branch — rename would silently REPLACE it.
    match crate::mailbox::exclusive_atomic_write(&path, &bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return compare_existing(context, &path, &would_be, pretty);
        }
        Err(error) => return Err(AppError::io("create owner config", &path, error)),
    }

    let sigs = would_be.sidecar_dir.join("sigs");
    fs::create_dir_all(&sigs)
        .map_err(|error| AppError::io("create owner sigs directory", &sigs, error))?;
    let output = OwnerInitOutput {
        ok: true,
        created: true,
        already_configured: false,
        owner: owner_json(&would_be),
        missing: missing_material(&would_be),
        guide: GUIDE,
    };
    Ok(CommandResult::json(&output, pretty)?.registration_committed())
}

/// The existing-destination branch: parse what is there and either confirm
/// idempotent equality (resolved comparison, not byte comparison) or refuse
/// loudly, printing the parse error or the differing fields. Nothing is
/// ever overwritten or repaired.
fn compare_existing(
    context: &Context,
    path: &Path,
    would_be: &ResolvedOwner,
    pretty: bool,
) -> AppResult<CommandResult> {
    // A malformed file is refused with its parse error; a symlink is refused
    // here too (read_owner_file never follows).
    let existing_file = crate::mailbox::read_owner_file(path)?;
    let rooms = context.load_rooms()?;
    let existing = crate::mailbox::resolve_owner_file(context, &existing_file, &rooms)?;
    if existing == *would_be {
        let output = OwnerInitOutput {
            ok: true,
            created: false,
            already_configured: true,
            owner: owner_json(&existing),
            missing: missing_material(&existing),
            guide: GUIDE,
        };
        return CommandResult::json(&output, pretty);
    }
    let diffs = diff_owners(would_be, &existing);
    Err(AppError::new(
        ErrorCode::ConfigInvalid,
        format!(
            "owner.json at '{}' already exists with a DIFFERENT configuration",
            path.display()
        ),
        "Resolve the conflict by hand (edit owner.json), or remove it and run `post owner init` again; owner rotation gets its own deliberate verb later.",
    )
    .reason(format!("differing fields: {}", diffs.join(", ")))
    .path(path.display().to_string()))
}

fn diff_owners(left: &ResolvedOwner, right: &ResolvedOwner) -> Vec<String> {
    let mut diffs = Vec::new();
    for (name, left_value, right_value) in [
        ("room", left.room.clone(), right.room.clone()),
        (
            "sidecar_dir",
            left.sidecar_dir.display().to_string(),
            right.sidecar_dir.display().to_string(),
        ),
        (
            "allowed_signers",
            left.allowed_signers.display().to_string(),
            right.allowed_signers.display().to_string(),
        ),
        ("principal", left.principal.clone(), right.principal.clone()),
        ("namespace", left.namespace.clone(), right.namespace.clone()),
        ("marker", left.marker.clone(), right.marker.clone()),
        ("label", left.label.clone(), right.label.clone()),
    ] {
        if left_value != right_value {
            diffs.push(name.to_owned());
        }
    }
    diffs
}

/// What still blocks verification, as a human-readable checklist: absent
/// allowed_signers file and absent signature pair (sigs/ holds no .sig).
/// Valid-config-missing-key-material is runtime absence, never ConfigInvalid.
fn missing_material(owner: &ResolvedOwner) -> Vec<String> {
    let mut missing = Vec::new();
    if !owner.allowed_signers.is_file() {
        missing.push("allowed_signers file (porch's onboarding writes the signer line)".to_owned());
    }
    let sigs = owner.sidecar_dir.join("sigs");
    let has_pair = fs::read_dir(&sigs)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("sig")
            })
        })
        .unwrap_or(false);
    if !has_pair {
        missing.push(
            "signing key pair (porch's onboarding generates the key; post only verifies)"
                .to_owned(),
        );
    }
    missing
}

/// `post owner show`: the resolved, post-derivation owner as JSON. A
/// malformed owner.json is ConfigInvalid here (Decision 3), never a partial
/// render.
fn show(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let resolution = load_owner(context)?;
    let (state, owner, note) = match &resolution {
        OwnerResolution::Configured(owner) => ("configured", Some(owner_json(owner)), None),
        OwnerResolution::Legacy(owner) => (
            "legacy",
            Some(owner_json(owner)),
            Some(format!(
                "legacy fallback ({}); consider `post owner init`.",
                owner.room
            )),
        ),
        OwnerResolution::None => ("none", None, Some("no signed owner configured".to_owned())),
    };
    let output = OwnerShowOutput {
        ok: true,
        state,
        owner,
        note,
    };
    CommandResult::json(&output, pretty)
}

/// The resolved owner as the stable JSON surface shared by init and show.
#[derive(Serialize)]
struct OwnerJson<'a> {
    room: &'a str,
    sidecar_dir: &'a Path,
    allowed_signers: &'a Path,
    principal: &'a str,
    namespace: &'a str,
    marker: &'a str,
    label: &'a str,
}

fn owner_json(owner: &ResolvedOwner) -> OwnerJson<'_> {
    OwnerJson {
        room: &owner.room,
        sidecar_dir: &owner.sidecar_dir,
        allowed_signers: &owner.allowed_signers,
        principal: &owner.principal,
        namespace: &owner.namespace,
        marker: &owner.marker,
        label: &owner.label,
    }
}

#[derive(Serialize)]
struct OwnerInitOutput<'a> {
    ok: bool,
    created: bool,
    already_configured: bool,
    owner: OwnerJson<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
    guide: &'static str,
}

#[derive(Serialize)]
struct OwnerShowOutput<'a> {
    ok: bool,
    /// configured | legacy | none (Decision 2 resolution states).
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<OwnerJson<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}
