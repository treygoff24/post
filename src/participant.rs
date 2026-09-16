use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, declared_env_pin, declared_sender_address, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(crate) const PARTICIPANTS_DIR: &str = "participants";
pub(crate) const PARTICIPANTS_LOCK_FILE: &str = ".participants.lock";
const RECORD_FILE: &str = "participant.json";
const RECORD_VERSION: u64 = 1;
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const FIX_LINE: &str = "run: post participant bind";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Participant {
    pub version: u64,
    pub id: String,
    pub harness: String,
    pub conversation_key_digest: String,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Provenance {
    ExplicitEnv,
    HarnessClaude,
    HarnessCodex,
    LauncherAddress,
}

impl Provenance {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitEnv => "explicit-env",
            Self::HarnessClaude => "harness-claude",
            Self::HarnessCodex => "harness-codex",
            Self::LauncherAddress => "launcher-address",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    Bound {
        participant: Box<Participant>,
        provenance: Provenance,
    },
    Unbound,
}

impl Resolved {
    pub(crate) fn participant(&self) -> Option<&Participant> {
        match self {
            Self::Bound { participant, .. } => Some(participant),
            Self::Unbound => None,
        }
    }

    pub(crate) fn provenance(&self) -> Option<Provenance> {
        match self {
            Self::Bound { provenance, .. } => Some(*provenance),
            Self::Unbound => None,
        }
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct Sender {
    pub from: String,
    pub participant: Participant,
    pub lineage: Option<String>,
}

#[derive(Debug)]
struct ConversationBinding {
    harness: String,
    key: String,
    provenance: Provenance,
}

pub(crate) fn resolve(context: &Context) -> AppResult<Resolved> {
    if let Some(explicit) = env_utf8("POST_PARTICIPANT")? {
        validate_participant_id(&explicit)?;
        return Ok(match load(context, &explicit)? {
            Some(participant) => Resolved::Bound {
                participant: Box::new(participant),
                provenance: Provenance::ExplicitEnv,
            },
            None => Resolved::Unbound,
        });
    }

    let Some(binding) = conversation_binding()? else {
        return Ok(Resolved::Unbound);
    };
    let digest = digest(&binding.key);
    if let Some(indexed) = read_index(context, &binding.harness, &digest)? {
        if let Some(participant) = load(context, &indexed)? {
            if participant.harness == binding.harness
                && participant.conversation_key_digest == digest
            {
                return Ok(Resolved::Bound {
                    participant: Box::new(participant),
                    provenance: binding.provenance,
                });
            }
        }
        return Ok(Resolved::Unbound);
    }

    for width in [8, 12] {
        let id = participant_id(&binding.harness, &digest, width);
        match load(context, &id)? {
            Some(participant) if participant.conversation_key_digest == digest => {
                return Ok(Resolved::Bound {
                    participant: Box::new(participant),
                    provenance: binding.provenance,
                });
            }
            Some(_) if width == 8 => continue,
            _ => return Ok(Resolved::Unbound),
        }
    }
    Ok(Resolved::Unbound)
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn require(context: &Context) -> AppResult<(Participant, Provenance)> {
    match resolve(context)? {
        Resolved::Bound {
            participant,
            provenance,
        } => Ok((*participant, provenance)),
        Resolved::Unbound => Err(AppError::no_participant(FIX_LINE)),
    }
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn sender(context: &Context) -> AppResult<Sender> {
    let (participant, _) = require(context)?;
    let from = participant
        .workspace
        .clone()
        .unwrap_or_else(|| participant.id.clone());
    let lineage = participant.lineage.clone();
    Ok(Sender {
        from,
        participant,
        lineage,
    })
}

/// The only participant-minting path. The record is committed before its
/// by-session cache entry while the one participant lock is held.
pub(crate) fn bind(
    context: &Context,
    cwd: &Path,
    workspace_override: Option<&str>,
) -> AppResult<Participant> {
    if let Some(explicit) = env_utf8("POST_PARTICIPANT")? {
        validate_participant_id(&explicit)?;
        return load(context, &explicit)?.ok_or_else(|| AppError::no_participant(FIX_LINE));
    }
    let binding = conversation_binding()?.ok_or_else(|| AppError::no_participant(FIX_LINE))?;
    let digest = digest(&binding.key);
    let (workspace, workspace_path) = workspace_context(context, cwd, workspace_override)?;

    let _lock = lock(context)?;
    let (id, mut participant) = select_record(context, &binding.harness, &digest)?;
    if let Some(existing) = participant.as_mut() {
        existing.workspace = workspace;
        existing.workspace_path = workspace_path;
        write_record(existing)?;
    } else {
        let (_, created) = crate::mailbox::local_timestamp()?;
        let dir = context.root.join(PARTICIPANTS_DIR).join(&id);
        fs::create_dir_all(&dir)
            .map_err(|error| AppError::io("create participant directory", &dir, error))?;
        let created_participant = Participant {
            version: RECORD_VERSION,
            id,
            harness: binding.harness.clone(),
            conversation_key_digest: digest.clone(),
            created,
            workspace,
            workspace_path,
            lineage: None,
            lineage_since: None,
            display_name: None,
            dir,
        };
        write_record(&created_participant)?;
        participant = Some(created_participant);
    }
    let participant = participant.expect("selected participant record exists");
    write_index(context, &binding.harness, &digest, &participant.id)?;
    Ok(participant)
}

pub(crate) fn lock(context: &Context) -> AppResult<File> {
    fs::create_dir_all(&context.root)
        .map_err(|error| AppError::io("create mailbox root", &context.root, error))?;
    let path = context.root.join(PARTICIPANTS_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| AppError::io("open participants lock", &path, error))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(AppError::io(
            "lock participants registry",
            &path,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(file)
}

pub(crate) fn list(context: &Context) -> AppResult<Vec<Participant>> {
    let root = context.root.join(PARTICIPANTS_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io("list participants", &root, error)),
    };
    let mut participants = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read participant entry", &root, error))?;
        if !entry
            .file_type()
            .map_err(|error| AppError::io("inspect participant entry", &entry.path(), error))?
            .is_dir()
        {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if id == "by-session" {
            continue;
        }
        if let Some(participant) = load(context, &id)? {
            participants.push(participant);
        }
    }
    participants.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(participants)
}

pub(crate) fn load(context: &Context, id: &str) -> AppResult<Option<Participant>> {
    validate_participant_id(id)?;
    let dir = context.root.join(PARTICIPANTS_DIR).join(id);
    let path = dir.join(RECORD_FILE);
    let Some(bytes) = read_bounded_optional(&path, MAX_RECORD_BYTES)? else {
        return Ok(None);
    };
    let mut participant: Participant = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(&path, format!("invalid participant JSON: {error}")))?;
    if participant.version != RECORD_VERSION {
        return Err(AppError::config(
            &path,
            format!("unsupported participant version {}", participant.version),
        ));
    }
    if participant.id != id {
        return Err(AppError::config(
            &path,
            format!(
                "participant id '{}' does not match its directory '{id}'",
                participant.id
            ),
        ));
    }
    validate_harness(&participant.harness)?;
    if participant.conversation_key_digest.len() != 64
        || !participant
            .conversation_key_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::config(
            &path,
            "conversation_key_digest must be 64 hexadecimal characters",
        ));
    }
    participant.dir = dir;
    Ok(Some(participant))
}

fn select_record(
    context: &Context,
    harness: &str,
    digest: &str,
) -> AppResult<(String, Option<Participant>)> {
    for width in [8, 12] {
        let id = participant_id(harness, digest, width);
        match load(context, &id)? {
            Some(participant) if participant.conversation_key_digest == digest => {
                return Ok((id, Some(participant)));
            }
            Some(_) if width == 8 => continue,
            Some(_) => {
                return Err(AppError::new(
                    ErrorCode::ConfigInvalid,
                    format!("participant id collision remains at 12 digest characters for '{id}'"),
                    "Inspect the two participant records and preserve both before retrying.",
                ))
            }
            None => return Ok((id, None)),
        }
    }
    unreachable!("the 8/12-width selection always returns")
}

fn write_record(participant: &Participant) -> AppResult<()> {
    let path = participant.dir.join(RECORD_FILE);
    let mut bytes = serde_json::to_vec_pretty(participant)
        .map_err(|error| AppError::io("serialize participant record", &path, error))?;
    bytes.push(b'\n');
    atomic_replace(&path, &bytes)
        .map_err(|error| AppError::io("atomically write participant record", &path, error))
}

fn index_path(context: &Context, harness: &str, digest: &str) -> PathBuf {
    context
        .root
        .join(PARTICIPANTS_DIR)
        .join("by-session")
        .join(harness)
        .join(digest)
}

fn read_index(context: &Context, harness: &str, digest: &str) -> AppResult<Option<String>> {
    let path = index_path(context, harness, digest);
    let Some(bytes) = read_bounded_optional(&path, 256)? else {
        return Ok(None);
    };
    let id = std::str::from_utf8(&bytes)
        .map_err(|error| AppError::config(&path, format!("index is not UTF-8: {error}")))?
        .trim();
    validate_participant_id(id)?;
    Ok(Some(id.to_owned()))
}

fn write_index(context: &Context, harness: &str, digest: &str, id: &str) -> AppResult<()> {
    let path = index_path(context, harness, digest);
    let parent = path.parent().expect("index has a parent");
    fs::create_dir_all(parent)
        .map_err(|error| AppError::io("create participant session index", parent, error))?;
    atomic_replace(&path, format!("{id}\n").as_bytes())
        .map_err(|error| AppError::io("atomically write participant session index", &path, error))
}

fn read_bounded_optional(path: &Path, maximum: u64) -> AppResult<Option<Vec<u8>>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("open participant state", path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| AppError::io("inspect participant state", path, error))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(AppError::config(
            path,
            format!("participant state must be a regular file no larger than {maximum} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| AppError::io("read participant state", path, error))?;
    Ok(Some(bytes))
}

fn conversation_binding() -> AppResult<Option<ConversationBinding>> {
    let harness_override = env_utf8("POST_HARNESS")?;
    if let Some(key) = env_utf8("CLAUDE_CODE_SESSION_ID")? {
        return Ok(Some(ConversationBinding {
            harness: harness_slug(harness_override.as_deref().unwrap_or("claude"))?,
            key,
            provenance: Provenance::HarnessClaude,
        }));
    }
    let thread = env_utf8("CODEX_THREAD_ID")?;
    let session = env_utf8("CODEX_SESSION_ID")?;
    if let (Some(thread), Some(session)) = (&thread, &session) {
        if thread != session {
            return Err(AppError::invalid_argument(
                "CODEX_THREAD_ID and CODEX_SESSION_ID are both set but differ; refusing to guess the conversation key",
            )
            .reason("conflicting Codex conversation keys"));
        }
    }
    if let Some(key) = thread.or(session) {
        return Ok(Some(ConversationBinding {
            harness: harness_slug(harness_override.as_deref().unwrap_or("codex"))?,
            key,
            provenance: Provenance::HarnessCodex,
        }));
    }
    let Some(address) = declared_sender_address()? else {
        return Ok(None);
    };
    let mut pieces = address.split('.');
    let Some(harness) = pieces.next() else {
        return Ok(None);
    };
    let Some(_repo_key) = pieces.next() else {
        return Err(invalid_launcher_address(&address));
    };
    let Some(key) = pieces.next() else {
        return Err(invalid_launcher_address(&address));
    };
    if pieces.next().is_some() || harness.is_empty() || key.is_empty() {
        return Err(invalid_launcher_address(&address));
    }
    Ok(Some(ConversationBinding {
        harness: harness_slug(harness)?,
        key: key.to_owned(),
        provenance: Provenance::LauncherAddress,
    }))
}

fn invalid_launcher_address(address: &str) -> AppError {
    AppError::invalid_argument(format!(
        "POST_SENDER_ADDRESS '{address}' must be <harness>.<repo-key>.<uuid> to bind a participant"
    ))
    .input(address)
    .reason("launcher address has the wrong shape")
}

fn workspace_context(
    context: &Context,
    cwd: &Path,
    workspace_override: Option<&str>,
) -> AppResult<(Option<String>, Option<PathBuf>)> {
    let rooms = context.load_rooms()?;
    let pinned = match workspace_override {
        Some(value) => Some(value.to_owned()),
        None => declared_env_pin()?,
    };
    if let Some(name) = pinned {
        let stored = rooms.get(&name).ok_or_else(|| {
            AppError::new(
                ErrorCode::UnknownRoom,
                format!("workspace '{name}' is not registered"),
                "Run `post rooms` and bind with a registered --workspace room.",
            )
            .input(name.clone())
            .reason("workspace is absent from rooms.json")
        })?;
        let path = context
            .expand_room_path(stored)
            .map_err(|reason| AppError::config(&context.root.join("rooms.json"), reason))?;
        return Ok((Some(name), Some(realpath_or_original(&path))));
    }

    let cwd = realpath_or_original(cwd);
    let mut matches = Vec::new();
    for (name, stored) in rooms {
        let path = context
            .expand_room_path(&stored)
            .map_err(|reason| AppError::config(&context.root.join("rooms.json"), reason))?;
        let path = realpath_or_original(&path);
        if cwd.starts_with(&path) {
            matches.push((path.components().count(), name, path));
        }
    }
    matches.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    Ok(match matches.pop() {
        Some((_, name, path)) => (Some(name), Some(path)),
        None => (None, None),
    })
}

fn realpath_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn env_utf8(name: &str) -> AppResult<Option<String>> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(None);
    };
    raw.into_string().map(Some).map_err(|_| {
        AppError::invalid_argument(format!("{name} is set but is not valid UTF-8"))
            .reason("non-UTF-8 environment value")
    })
}

fn harness_slug(value: &str) -> AppResult<String> {
    validate_harness(value)?;
    Ok(value.to_ascii_lowercase())
}

fn validate_harness(value: &str) -> AppResult<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(AppError::invalid_argument(format!(
            "harness slug '{value}' must contain only ASCII letters, digits, '-' or '_'"
        ))
        .input(value)
        .reason("invalid harness slug"));
    }
    Ok(())
}

fn validate_participant_id(value: &str) -> AppResult<()> {
    crate::mailbox::validate_component(value).map_err(|reason| {
        AppError::invalid_argument(format!("participant id '{value}' is invalid: {reason}"))
            .input(value)
            .reason(reason)
    })?;
    if value.contains(':') {
        return Err(AppError::invalid_argument(format!(
            "participant id '{value}' must not contain ':'"
        )));
    }
    Ok(())
}

fn participant_id(harness: &str, digest: &str, width: usize) -> String {
    format!("{harness}-{}", &digest[..width])
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
