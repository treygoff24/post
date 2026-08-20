//! The small write-admission fence used while one mailbox store is migrated.
//!
//! Legacy stores have no state file and no generation declaration.  Once a
//! migration writes `.post-arx.json`, writers must present the generation in
//! `POST_ARX_GENERATION`; readers never need that declaration.

use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, Context};
use serde::de::{self, MapAccess, Visitor};
use serde::Deserialize;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

pub(crate) const STATE_FILE: &str = ".post-arx.json";
pub(crate) const LOCK_FILE: &str = ".post-arx.lock";
pub(crate) const GENERATION_ENV: &str = "POST_ARX_GENERATION";

const MAX_STATE_BYTES: u64 = 4096;

#[cfg(test)]
type LockOpenHook = Box<dyn Fn(&Path) + Send>;

#[cfg(test)]
static LOCK_OPEN_HOOK: OnceLock<Mutex<Option<LockOpenHook>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FenceState {
    Fenced { generation: u64 },
    Active { generation: u64 },
}

#[derive(Debug)]
pub(crate) struct WriteAdmission {
    _lock: Option<File>,
    enrolled: bool,
}

impl WriteAdmission {
    pub(crate) fn is_enrolled(&self) -> bool {
        self.enrolled
    }
}

#[derive(Debug)]
struct StateFile {
    state: String,
    generation: Option<u64>,
}

// serde_json otherwise accepts duplicate object keys.  A fence file with two
// state or generation claims is ambiguous, so reject it instead of choosing
// whichever value happened to come last.
impl<'de> Deserialize<'de> for StateFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StateFileVisitor;

        impl<'de> Visitor<'de> for StateFileVisitor {
            type Value = StateFile;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique state and generation fields")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut state = None;
                let mut generation = None;
                let mut saw_state = false;
                let mut saw_generation = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "state" if !saw_state => {
                            saw_state = true;
                            state = Some(map.next_value()?);
                        }
                        "generation" if !saw_generation => {
                            saw_generation = true;
                            generation = Some(map.next_value()?);
                        }
                        "state" => return Err(de::Error::duplicate_field("state")),
                        "generation" => return Err(de::Error::duplicate_field("generation")),
                        _ => return Err(de::Error::unknown_field(&key, &["state", "generation"])),
                    }
                }
                Ok(StateFile {
                    state: state.ok_or_else(|| de::Error::missing_field("state"))?,
                    generation,
                })
            }
        }

        deserializer.deserialize_map(StateFileVisitor)
    }
}

impl StateFile {
    fn into_state(self, path: &Path) -> AppResult<FenceState> {
        let state = match self.state.as_str() {
            "fenced" => FenceState::Fenced {
                generation: valid_generation(path, self.generation)?,
            },
            "active" => FenceState::Active {
                generation: valid_generation(path, self.generation)?,
            },
            _ => {
                return Err(invalid_state(
                    path,
                    "state must be fenced or active with a positive generation",
                ))
            }
        };
        Ok(state)
    }
}

fn valid_generation(path: &Path, generation: Option<u64>) -> AppResult<u64> {
    generation
        .filter(|generation| *generation > 0)
        .ok_or_else(|| {
            invalid_state(
                path,
                "fenced and active states require a positive generation",
            )
        })
}

fn invalid_state(path: &Path, reason: &str) -> AppError {
    AppError::new(
        ErrorCode::ConfigInvalid,
        format!("migration fence '{}' is invalid: {reason}", path.display()),
        "Repair or complete the migration fence before retrying a write; read-only commands remain available.",
    )
    .path(path.display().to_string())
    .reason(reason)
}

fn state_path(context: &Context) -> PathBuf {
    context.root.join(STATE_FILE)
}

fn lock_path(context: &Context) -> PathBuf {
    context.root.join(LOCK_FILE)
}

fn current_generation() -> AppResult<Option<u64>> {
    let Some(raw) = std::env::var_os(GENERATION_ENV) else {
        return Ok(None);
    };
    let value = raw.to_str().ok_or_else(|| {
        AppError::new(
            ErrorCode::ConfigInvalid,
            format!("migration fence: {GENERATION_ENV} is set but is not valid UTF-8"),
            format!("Unset {GENERATION_ENV} or set it to the active positive generation."),
        )
        .reason("non-UTF-8 generation declaration")
    })?;
    let generation = value.parse::<u64>().ok().filter(|value| *value > 0);
    generation.ok_or_else(|| {
        AppError::new(
            ErrorCode::ConfigInvalid,
            format!("migration fence: {GENERATION_ENV} must be a positive integer when set"),
            format!("Unset {GENERATION_ENV} for a legacy store or set the active positive generation."),
        )
        .input(GENERATION_ENV)
        .reason("invalid generation declaration")
    }).map(Some)
}

fn lock(context: &Context, create: bool) -> AppResult<File> {
    let path = lock_path(context);
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    if create {
        options.create(true).mode(0o600);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error)
            if !create
                && (error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ELOOP)) =>
        {
            return Err(invalid_state(
                &path,
                "lock file is missing or not a trusted regular file",
            ));
        }
        Err(error) => return Err(AppError::io("open migration fence lock", &path, error)),
    };
    let before = file
        .metadata()
        .map_err(|error| AppError::io("inspect migration fence lock", &path, error))?;
    if !trusted_lock_metadata(&before) {
        return Err(invalid_state(&path, "lock must be a solitary regular file"));
    }
    #[cfg(test)]
    let hook = LOCK_OPEN_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("lock-open hook mutex")
        .take();
    #[cfg(test)]
    if let Some(hook) = hook {
        hook(&path);
        *LOCK_OPEN_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("lock-open hook mutex") = Some(hook);
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(AppError::io(
            "lock migration fence",
            &path,
            std::io::Error::last_os_error(),
        ));
    }
    let after = file
        .metadata()
        .map_err(|error| AppError::io("reinspect migration fence lock", &path, error))?;
    let on_path = std::fs::symlink_metadata(&path).map_err(|error| {
        invalid_state(
            &path,
            if error.kind() == std::io::ErrorKind::NotFound {
                "lock file disappeared while acquiring its flock"
            } else {
                "lock path cannot be inspected after acquiring its flock"
            },
        )
    })?;
    if !trusted_lock_metadata(&after)
        || !trusted_lock_metadata(&on_path)
        || after.dev() != on_path.dev()
        || after.ino() != on_path.ino()
        || after.nlink() != on_path.nlink()
    {
        return Err(invalid_state(
            &path,
            "lock path changed while acquiring its flock",
        ));
    }
    Ok(file)
}

fn trusted_lock_metadata(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_file() && metadata.nlink() == 1
}

fn read_state(context: &Context) -> AppResult<Option<FenceState>> {
    let path = state_path(context);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(invalid_state(&path, "state file is a symlink"))
        }
        Err(error) => return Err(AppError::io("open migration fence", &path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| AppError::io("inspect migration fence", &path, error))?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(invalid_state(
            &path,
            "state must be a solitary regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| AppError::io("read migration fence", &path, error))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid_state(&path, "state exceeds the 4 KiB limit"));
    }
    let parsed: StateFile = serde_json::from_slice(&bytes)
        .map_err(|error| invalid_state(&path, &format!("invalid JSON: {error}")))?;
    parsed.into_state(&path).map(Some)
}

pub(crate) fn read_only_must_not_mutate(context: &Context) -> bool {
    // Presence is enough for reads: never parse or reject a writer-only
    // declaration on a read path. A set declaration also protects a missing
    // root/state pair from legacy first-run initialization.
    if std::env::var_os(GENERATION_ENV).is_some() {
        return true;
    }
    matches!(
        read_state(context),
        Ok(Some(FenceState::Fenced { .. } | FenceState::Active { .. })) | Err(_)
    )
}

fn refuse(context: &Context, reason: impl Into<String>) -> AppError {
    let path = state_path(context);
    AppError::new(
        ErrorCode::ConfigInvalid,
        format!(
            "write refused by migration fence '{}': {}",
            path.display(),
            reason.into()
        ),
        format!("Complete the migration or run this writer with the current {GENERATION_ENV}."),
    )
    .path(path.display().to_string())
    .reason("migration generation is not admitted")
}

pub(crate) fn admit(context: &Context, writes: bool) -> AppResult<WriteAdmission> {
    if !writes {
        return Ok(WriteAdmission {
            _lock: None,
            enrolled: false,
        });
    }
    admit_generation(context, true, current_generation()?)
}

fn admit_generation(
    context: &Context,
    writes: bool,
    generation: Option<u64>,
) -> AppResult<WriteAdmission> {
    if !writes {
        return Ok(WriteAdmission {
            _lock: None,
            enrolled: false,
        });
    }
    if !context.root.exists() {
        if generation.is_some() {
            return Err(refuse(context, "the enrolled state file is missing"));
        }
        return Ok(WriteAdmission {
            _lock: None,
            enrolled: false,
        });
    }
    // Legacy writers deliberately skip the enrollment-owned lock. Cutover
    // drains in-flight legacy writers before copying and activates the state.
    if read_state(context)?.is_none() {
        return if generation.is_none() {
            Ok(WriteAdmission {
                _lock: None,
                enrolled: false,
            })
        } else {
            Err(refuse(context, "the enrolled state file is missing"))
        };
    }

    // Parse before opening the existing lock so a malformed/symlinked fence
    // refuses without creating an admission artifact behind the refusal.
    // Re-read after locking to close the state-change race, then retain that
    // same trusted lock through the command's mutation.
    let lock = lock(context, false)?;
    match read_state(context)? {
        None => Err(refuse(context, "the enrolled state file disappeared")),
        Some(FenceState::Fenced { .. }) => Err(refuse(context, "the store is fenced")),
        Some(FenceState::Active {
            generation: expected,
        }) => match generation {
            Some(actual) if actual == expected => Ok(WriteAdmission {
                _lock: Some(lock),
                enrolled: true,
            }),
            Some(actual) => Err(refuse(
                context,
                format!("writer generation {actual} is stale; current generation is {expected}"),
            )),
            None => Err(refuse(
                context,
                "an active store requires an explicit writer generation",
            )),
        },
    }
}

fn write_state_locked(context: &Context, state: FenceState) -> AppResult<()> {
    let path = state_path(context);
    let (state, generation) = match state {
        FenceState::Fenced { generation } => ("fenced", Some(generation)),
        FenceState::Active { generation } => ("active", Some(generation)),
    };
    let value = match generation {
        Some(generation) => serde_json::json!({
            "state": state,
            "generation": generation,
        }),
        None => serde_json::json!({"state": state}),
    };
    let mut bytes = serde_json::to_vec_pretty(&value)
        .map_err(|error| AppError::io("serialize migration fence", &path, error))?;
    bytes.push(b'\n');
    atomic_replace(&path, &bytes)
        .map_err(|error| AppError::io("atomically update migration fence", &path, error))
}

#[allow(dead_code)]
pub(crate) fn fence(context: &Context, generation: u64) -> AppResult<()> {
    if generation == 0 {
        return Err(AppError::invalid_argument(
            "migration generation must be positive",
        ));
    }
    if read_state(context)?.is_some() {
        return Err(refuse(context, "fencing is only legal before enrollment"));
    }
    let lock = lock(context, true)?;
    let prior = read_state(context)?;
    if prior.is_some() {
        return Err(refuse(context, "fencing is only legal before enrollment"));
    }
    write_state_locked(context, FenceState::Fenced { generation })?;
    drop(lock);
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn activate(context: &Context, generation: u64) -> AppResult<()> {
    let lock = lock(context, false)?;
    if !matches!(read_state(context)?, Some(FenceState::Fenced { generation: expected }) if expected == generation)
    {
        return Err(refuse(
            context,
            "activation requires a fenced state with the same generation",
        ));
    }
    write_state_locked(context, FenceState::Active { generation })?;
    drop(lock);
    Ok(())
}

pub(crate) fn classify_write(command: &crate::cli::Command) -> bool {
    use crate::cli::{Command, DoctorArgs};
    match command {
        Command::Doctor(DoctorArgs { fix: true })
        | Command::Send(_)
        | Command::Rooms(crate::cli::RoomsArgs {
            command: Some(crate::cli::RoomsCommand::Add(_)),
        })
        | Command::Owner(crate::cli::OwnerArgs {
            command: Some(crate::cli::OwnerCommand::Init(_)),
        })
        | Command::Profile(crate::cli::ProfileArgs {
            command: Some(crate::cli::ProfileCommand::Set(_) | crate::cli::ProfileCommand::Clear),
        }) => true,
        Command::Read(args) => !args.peek,
        Command::Chat(args) => {
            args.join
                || args.send
                || args.body.is_some()
                || args.body_file.is_some()
                || args.file.is_some()
                || args.discard
                || args.discard_through.is_some()
                || (!args.peek
                    && args.history.is_none()
                    && args.since.is_none()
                    && args.seen_by.is_none())
        }
        Command::Watch(args) => !args.snapshot,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::test_support::{test_root, trash_test_root};
    use clap::Parser;
    use std::fs;
    use std::sync::mpsc;
    use std::thread;

    fn parse(args: &[&str]) -> crate::cli::Command {
        Cli::try_parse_from(args).expect("valid CLI").command
    }

    fn context(label: &str) -> (PathBuf, Context) {
        let root = test_root(label);
        fs::create_dir_all(&root).expect("root");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        (root, context)
    }

    #[test]
    fn classifies_every_writer_and_read_only_variant() {
        for args in [
            &["post", "doctor", "--fix"] as &[&str],
            &["post", "send", "--to", "beta", "--body", "x"],
            &["post", "read", "id"],
            &["post", "chat", "tax", "--join"],
            &["post", "chat", "tax", "--send", "--body", "x"],
            &["post", "chat", "tax", "--discard"],
            &["post", "chat", "tax", "--discard-through", "id"],
            &["post", "rooms", "add", "alpha", "/tmp"],
            &["post", "profile", "set", "--name", "x"],
            &["post", "profile", "clear"],
            &["post", "owner", "init", "--room", "alpha"],
            &["post", "watch"],
        ] {
            assert!(classify_write(&parse(args)), "writer: {args:?}");
        }
        for args in [
            &["post", "doctor"] as &[&str],
            &["post", "schema"],
            &["post", "channels"],
            &["post", "inbox"],
            &["post", "read", "id", "--peek"],
            &["post", "chat", "tax", "--peek"],
            &["post", "chat", "tax", "--history", "1"],
            &["post", "chat", "tax", "--since", "id"],
            &["post", "chat", "tax", "--seen-by", "id"],
            &["post", "rooms"],
            &["post", "profile", "show"],
            &["post", "owner", "show"],
            &["post", "watch", "--snapshot"],
            &["post", "who"],
        ] {
            assert!(!classify_write(&parse(args)), "read-only: {args:?}");
        }
    }

    #[test]
    fn transitions_are_locked_and_illegal_transitions_refuse() {
        let (root, context) = context("arx-transitions");
        assert!(fence(&context, 0).is_err());
        assert!(admit_generation(&context, true, None).is_ok());
        fence(&context, 7).expect("fence");
        assert!(fence(&context, 8).is_err());
        // A fenced store admits no writer, even if it declares the generation.
        assert!(admit_generation(&context, true, Some(7)).is_err());
        activate(&context, 7).expect("activate");
        assert!(admit_generation(&context, true, None).is_err());
        trash_test_root(&root);
    }

    #[test]
    fn active_generation_allows_current_and_refuses_stale_or_missing() {
        let (root, context) = context("arx-generation");
        fence(&context, 7).expect("fence");
        activate(&context, 7).expect("activate");
        assert!(admit_generation(&context, true, Some(7)).is_ok());
        assert!(admit_generation(&context, true, Some(6)).is_err());
        assert!(admit_generation(&context, true, None).is_err());
        trash_test_root(&root);
    }

    #[test]
    fn malformed_and_missing_enrolled_state_fail_closed_without_read_mutation() {
        let (root, context) = context("arx-fail-closed");
        fs::write(
            root.join(STATE_FILE),
            br#"{"state":"active","state":"fenced"}"#,
        )
        .expect("ambiguous state");
        assert!(admit_generation(&context, true, Some(7)).is_err());
        assert!(read_only_must_not_mutate(&context));

        fs::write(
            root.join(STATE_FILE),
            br#"{"state":"active","generation":7}"#,
        )
        .expect("active state");
        assert!(read_only_must_not_mutate(&context));
        let _guard = crate::mailbox::enter_read_only_command(true);
        context.mailbox_dirs("alpha").expect("read-only paths");
        assert!(!root.join("alpha").exists());

        fs::remove_file(root.join(STATE_FILE)).expect("remove enrolled state");
        assert!(admit_generation(&context, true, Some(7)).is_err());
        trash_test_root(&root);
    }

    #[test]
    fn legacy_first_run_still_allows_writer_defaults() {
        let root = test_root("arx-first-run");
        trash_test_root(&root);
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        assert!(admit_generation(&context, true, None).is_ok());
        context.prepare_first_run().expect("prepare defaults");
        assert!(root.join("rooms.json").is_file());
        assert!(root.join("rules.json").is_file());
        trash_test_root(&root);
    }

    #[test]
    fn lock_path_unlink_and_replace_are_refused_after_flock() {
        for replace in [false, true] {
            let label = if replace {
                "arx-lock-replace"
            } else {
                "arx-lock-unlink"
            };
            let (root, context) = context(label);
            let lock_path = root.join(LOCK_FILE);
            fs::write(&lock_path, b"").expect("lock");
            let holder = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("holder");
            assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);

            let (opened_tx, opened_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let expected_path = lock_path.clone();
            *LOCK_OPEN_HOOK
                .get_or_init(|| Mutex::new(None))
                .lock()
                .expect("lock-open hook mutex") = Some(Box::new(move |path| {
                if path == expected_path {
                    opened_tx.send(()).expect("signal lock open");
                    release_rx.recv().expect("release lock open");
                }
            }));

            let child_context = context.clone();
            let child = thread::spawn(move || lock(&child_context, false));
            opened_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("child opened old lock inode");
            if replace {
                fs::write(root.join("replacement"), b"").expect("replacement");
                fs::rename(root.join("replacement"), &lock_path).expect("replace lock path");
            } else {
                fs::remove_file(&lock_path).expect("unlink lock path");
            }
            drop(holder);
            release_tx.send(()).expect("release child");
            let result = child.join().expect("join lock child");
            assert!(
                result.is_err(),
                "split lock admission succeeded: {result:?}"
            );
            *LOCK_OPEN_HOOK
                .get_or_init(|| Mutex::new(None))
                .lock()
                .expect("lock-open hook mutex") = None;
            trash_test_root(&root);
        }
    }
}
