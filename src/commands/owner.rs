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

    let sigs = would_be.sidecar_dir.join("sigs");
    // Pre-commit ordering (A0a Decision 5): the sidecar scaffold is created
    // BEFORE owner.json commits, so a failed install (e.g. an unwritable
    // sidecar) leaves NO owner.json behind — the retry is a clean first
    // run, not an "already configured" trap with missing sigs. create_dir_all
    // is idempotent, so this weakens nothing about create-only semantics;
    // a refuse path above still returns before anything is created.
    fs::create_dir_all(&sigs)
        .map_err(|error| AppError::io("create owner sigs directory", &sigs, error))?;
    let mut bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| AppError::io("serialize owner config", &path, error))?;
    bytes.push(b'\n');
    // Create-only commit via the no-replace primitive: `create_new` temp,
    // write+sync (0600), then hard_link to the destination. A destination
    // created between the precheck above and this commit surfaces as
    // AlreadyExists and routes to the compare branch — rename would silently
    // REPLACE it.
    match crate::mailbox::exclusive_atomic_write(&path, &bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return compare_existing(context, &path, &would_be, pretty);
        }
        Err(error) => return Err(AppError::io("create owner config", &path, error)),
    }
    let output = OwnerInitOutput {
        ok: true,
        created: true,
        already_configured: false,
        owner: owner_json(&would_be),
        missing: missing_material(&would_be),
        note: sidecar_note(&would_be),
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
        // Complete a stranded install: an identical retry re-runs the sidecar
        // scaffold so a retry after a mid-install failure (or a config that
        // predates A0a) ends fully provisioned, never half-configured.
        let sigs = existing.sidecar_dir.join("sigs");
        fs::create_dir_all(&sigs)
            .map_err(|error| AppError::io("create owner sigs directory", &sigs, error))?;
        let output = OwnerInitOutput {
            ok: true,
            created: false,
            already_configured: true,
            owner: owner_json(&existing),
            missing: missing_material(&existing),
            note: sidecar_note(&existing),
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

/// What still blocks verification, as a human-readable checklist. Only
/// OBSERVABLE prerequisites are asserted: post can see the allowed_signers
/// file, but NEVER the signing key — porch generates and holds it (possibly
/// on another host), so no on-disk state here is ever claimed to be a "key
/// pair". Prior signed payloads are NOT onboarding material: their absence
/// does not block verifying the first future signed message, and a planted
/// `.sig` proves nothing, so payloads never appear as missing or as wired
/// evidence. Valid-config-missing-key-material is runtime absence, never
/// ConfigInvalid.
fn missing_material(owner: &ResolvedOwner) -> Vec<String> {
    let mut missing = Vec::new();
    if !owner.allowed_signers.is_file() {
        missing.push(
            "allowed_signers file (porch's onboarding writes the signer line; verification reads it)"
                .to_owned(),
        );
    }
    missing
}

/// A NON-BLOCKING, purely informational observation for onboarding output:
/// whether any signed message payloads have arrived yet. Absence implies
/// nothing about the wiring — porch signs the first message and post
/// verifies it — so this is never missing material and never evidence.
fn sidecar_note(owner: &ResolvedOwner) -> Option<&'static str> {
    let sigs = owner.sidecar_dir.join("sigs");
    let has_payloads = fs::read_dir(&sigs)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("sig")
            })
        })
        .unwrap_or(false);
    if has_payloads {
        None
    } else {
        Some(
            "no signed message payloads observed yet (sigs/ is empty) — not a blocker: porch signs the first message and post verifies it",
        )
    }
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
    /// Non-blocking observation (never missing material): whether any
    /// signed payloads have arrived yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
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

#[cfg(test)]
mod tests {
    use super::run;
    use crate::cli::{OwnerArgs, OwnerCommand, OwnerInitArgs};
    use crate::mailbox::{set_pre_commit_hook, Context};
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn context(label: &str) -> (PathBuf, Context) {
        let root = test_root(&format!("owner-init-{label}"));
        fs::write(root.join("rooms.json"), r#"{"mara": "~/.mara-room"}"#).expect("seed rooms");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        context.prepare_first_run().expect("defaults");
        (root, context)
    }

    fn init_args(room: &str) -> OwnerInitArgs {
        OwnerInitArgs {
            room: room.to_owned(),
            marker: None,
            label: None,
            sidecar_dir: None,
            allowed_signers: None,
            principal: None,
            namespace: None,
        }
    }

    fn run_init(
        context: &Context,
        room: &str,
    ) -> crate::error::AppResult<crate::command_result::CommandResult> {
        run(
            context,
            OwnerArgs {
                command: Some(OwnerCommand::Init(init_args(room))),
            },
            false,
        )
    }

    /// The ratified adversarial commit race, deterministically: a
    /// destination is planted AFTER the temp write+sync and BEFORE the
    /// hard-link commit (via the pre-commit seam in
    /// `exclusive_atomic_write_with`). The commit must refuse it
    /// (AlreadyExists), and the init flow must route to the SAME
    /// compare branch as a pre-existing file for identical, different, and
    /// malformed planted content — never replace it.
    #[test]
    fn owner_init_adversarial_race_plants_destination_before_commit() {
        let (root, context) = context("race");
        let path = root.join("owner.json");
        for (planted, expect_already_configured) in [
            (r#"{"room":"mara"}"#, true),
            (r#"{"room":"mara","label":"Other"}"#, false),
            ("{not json", false),
        ] {
            let _ = fs::remove_file(&path);
            let planted_bytes = planted.as_bytes().to_vec();
            let hook_path = path.clone();
            // Plant between the temp write and the hard-link commit.
            set_pre_commit_hook(Some(Box::new(move |_temporary: &Path| {
                fs::write(&hook_path, &planted_bytes)
            })));
            let outcome = run_init(&context, "mara");
            set_pre_commit_hook(None);
            match (outcome, expect_already_configured) {
                (Ok(result), true) => {
                    assert_eq!(result.exit_code, 0);
                    let json: serde_json::Value =
                        serde_json::from_str(&result.stdout).expect("init stdout");
                    assert_eq!(json["already_configured"], true);
                    assert_eq!(json["created"], false);
                }
                (Err(error), false) => {
                    assert_eq!(error.code.as_str(), "config_invalid");
                }
                (Ok(_), false) => panic!("raced different/malformed destination must refuse"),
                (Err(_), true) => panic!("identical raced destination must be idempotent"),
            }
            assert_eq!(
                fs::read_to_string(&path).expect("reread raced owner.json"),
                planted,
                "the racing writer's owner.json must be byte-untouched"
            );
        }
        set_pre_commit_hook(None);
        trash_test_root(&root);
    }

    /// A0b r3 item 3: `missing` names ONLY observable prerequisites. A
    /// fresh, fully-provisioned owner (allowed_signers present, no signed
    /// messages yet) has nothing missing; the empty sigs/ dir is reported as
    /// a separate NON-BLOCKING note. A planted garbage `.sig` must never
    /// count as evidence that signing is wired — with allowed_signers
    /// absent it stays missing, with it present nothing is missing.
    #[test]
    fn init_missing_names_only_observable_prerequisites() {
        let (root, context) = context("missing");
        let sidecar = root.join(".mara-room");
        fs::create_dir_all(&sidecar).expect("sidecar");
        let signers = |content: &str| {
            fs::write(sidecar.join("allowed_signers"), content).expect("allowed_signers")
        };
        let init_json = || -> serde_json::Value {
            let result = run_init(&context, "mara").expect("init");
            serde_json::from_str(&result.stdout).expect("init stdout")
        };

        // Fresh and fully provisioned, before the first send: nothing is
        // missing; sigs/ emptiness is a non-blocking observation only.
        signers("mara@porch ssh-ed25519 AAAA\n");
        let json = init_json();
        assert_eq!(json["created"], true);
        assert!(
            json.get("missing").is_none(),
            "fully provisioned owner must name nothing missing: {json}"
        );
        let note = json["note"].as_str().expect("non-blocking observation");
        assert!(
            note.contains("not a blocker"),
            "the payload observation must be non-blocking: {note}"
        );

        // A garbage .sig is planted; allowed_signers is now absent. The
        // planted sidecar must not masquerade as wiring evidence: missing
        // still names the allowed_signers prerequisite and nothing about
        // payloads.
        let sigs = sidecar.join("sigs");
        fs::create_dir_all(&sigs).expect("sigs dir");
        fs::write(sigs.join("garbage.sig"), b"not a signature").expect("plant garbage .sig");
        fs::remove_file(sidecar.join("allowed_signers")).expect("remove allowed_signers");
        let json = init_json();
        assert_eq!(json["already_configured"], true);
        let missing = json["missing"]
            .as_array()
            .expect("allowed_signers must be named missing");
        assert!(
            missing.iter().any(|item| item
                .as_str()
                .expect("string item")
                .contains("allowed_signers")),
            "the real prerequisite must stay missing: {missing:?}"
        );
        assert!(
            !missing.iter().any(|item| {
                let text = item.as_str().expect("string item");
                text.contains("payload")
                    || text.contains("signed message")
                    || text.contains("evidence")
            }),
            "a planted .sig must never count as wired evidence: {missing:?}"
        );

        // Restore allowed_signers with the garbage .sig still in place:
        // nothing missing again, and the observation note is gone (a
        // sidecar has been observed — the note only reports emptiness).
        signers("mara@porch ssh-ed25519 AAAA\n");
        let json = init_json();
        assert!(
            json.get("missing").is_none(),
            "planted garbage .sig must not count as evidence: {json}"
        );
        assert!(
            json.get("note").is_none(),
            "a .sig was observed, so the empty-sidecars note must not fire: {json}"
        );

        trash_test_root(&root);
    }
}
