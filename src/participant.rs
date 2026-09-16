use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, declared_env_pin, declared_sender_address, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const PARTICIPANTS_DIR: &str = "participants";
pub(crate) const PARTICIPANTS_LOCK_FILE: &str = ".participants.lock";
const RECORD_FILE: &str = "participant.json";
const RECORD_VERSION: u64 = 1;
const MAX_RECORD_BYTES: u64 = 64 * 1024;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AddressKind {
    Workspace,
    Lineage,
    Participant,
}

impl AddressKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Lineage => "lineage",
            Self::Participant => "participant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Address {
    pub kind: AddressKind,
    pub name: String,
}

impl Address {
    pub(crate) fn inbox(&self, context: &Context) -> AppResult<PathBuf> {
        let inbox = match self.kind {
            AddressKind::Workspace => context.mailbox_dirs(&self.name)?.0,
            AddressKind::Lineage => context
                .root
                .join(crate::lineage::LINEAGES_DIR)
                .join(&self.name)
                .join("inbox"),
            AddressKind::Participant => context
                .root
                .join(PARTICIPANTS_DIR)
                .join(&self.name)
                .join("inbox"),
        };
        fs::create_dir_all(&inbox)
            .map_err(|error| AppError::io("create canonical target inbox", &inbox, error))?;
        Ok(inbox)
    }
}

/// Resolve an explicit typed address or a bare name. Exact workspace names
/// win before typed parsing so legacy rooms containing ':' remain addressable.
pub(crate) fn resolve_target(context: &Context, raw: &str) -> AppResult<Address> {
    let rooms = context.load_rooms()?;
    if rooms.contains_key(raw) {
        return Ok(Address {
            kind: AddressKind::Workspace,
            name: raw.to_owned(),
        });
    }

    if let Some((prefix, name)) = raw.split_once(':') {
        if name.is_empty() {
            return Err(AppError::invalid_argument(format!(
                "typed target '{raw}' must contain a non-empty name after ':'"
            )));
        }
        let kind = match prefix {
            "workspace" if rooms.contains_key(name) => AddressKind::Workspace,
            "lineage" if crate::lineage::load(context, name)?.is_some() => AddressKind::Lineage,
            "participant" if load(context, name)?.is_some() => AddressKind::Participant,
            "workspace" | "lineage" | "participant" => {
                return Err(unknown_typed_target(prefix, name));
            }
            _ => {
                return Err(AppError::invalid_argument(format!(
                    "typed target prefix '{prefix}' is unknown; expected workspace, lineage, or participant"
                ))
                .input(raw)
                .reason("unknown typed target prefix"));
            }
        };
        return Ok(Address {
            kind,
            name: name.to_owned(),
        });
    }

    if crate::lineage::load(context, raw)?.is_some() {
        return Ok(Address {
            kind: AddressKind::Lineage,
            name: raw.to_owned(),
        });
    }
    if load(context, raw)?.is_some() {
        return Ok(Address {
            kind: AddressKind::Participant,
            name: raw.to_owned(),
        });
    }
    Err(AppError::new(
        ErrorCode::UnknownRoom,
        format!("recipient room '{raw}' is unknown"),
        "Run `post rooms`, `post identity list`, or `post participant list`, then retry with an existing target.",
    )
    .input(raw)
    .reason("target is absent"))
}

fn unknown_typed_target(kind: &str, name: &str) -> AppError {
    AppError::new(
        ErrorCode::NotFound,
        format!("{kind} target '{name}' does not exist in this local store"),
        "Run `post rooms`, `post identity list`, or `post participant list`, then retry with an existing target.",
    )
    .input(format!("{kind}:{name}"))
    .reason("typed target is absent")
}

#[derive(Debug)]
struct ConversationBinding {
    harness: String,
    key: String,
    provenance: Provenance,
}

pub(crate) fn resolve(context: &Context) -> AppResult<Resolved> {
    #[cfg(test)]
    if let Some(id) = test_actor_id(context) {
        if let Some(participant) = load(context, &id)? {
            return Ok(Resolved::Bound {
                participant: Box::new(participant),
                provenance: Provenance::ExplicitEnv,
            });
        }
    }
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

pub(crate) fn bind_key_available() -> AppResult<bool> {
    if env_utf8("POST_PARTICIPANT")?.is_some() {
        return Ok(false);
    }
    Ok(conversation_binding()?.is_some())
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn require(context: &Context) -> AppResult<(Participant, Provenance)> {
    match resolve(context)? {
        Resolved::Bound {
            participant,
            provenance,
        } => Ok((*participant, provenance)),
        Resolved::Unbound => Err(AppError::no_participant(bind_key_available()?)),
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
    bootstrap: Option<(&str, &str)>,
) -> AppResult<Participant> {
    let explicit = env_utf8("POST_PARTICIPANT")?;
    if bootstrap.is_some() && explicit.is_some() {
        return Err(AppError::invalid_argument(
            "--key/--new cannot be combined with POST_PARTICIPANT; unset it to mint a different participant",
        ));
    }
    if let Some(explicit) = explicit {
        validate_participant_id(&explicit)?;
        if workspace_override.is_none() && declared_env_pin()?.is_none() {
            return load(context, &explicit)?.ok_or_else(|| AppError::no_participant(false));
        }
        let (workspace, workspace_path) = workspace_context(context, cwd, workspace_override)?;
        let _lock = lock(context)?;
        let mut participant =
            load(context, &explicit)?.ok_or_else(|| AppError::no_participant(false))?;
        participant.workspace = workspace;
        participant.workspace_path = workspace_path;
        write_record(&participant)?;
        return Ok(participant);
    }
    let binding = match bootstrap {
        Some((harness, key)) => ConversationBinding {
            harness: harness_slug(harness)?,
            key: key.to_owned(),
            // Bind output names the explicit result directly; subsequent
            // invocations resolve through POST_PARTICIPANT.
            provenance: Provenance::ExplicitEnv,
        },
        None => conversation_binding()?.ok_or_else(|| AppError::no_participant(false))?,
    };
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

pub(crate) fn fresh_uuid_key() -> AppResult<String> {
    let path = Path::new("/dev/urandom");
    let mut bytes = [0_u8; 16];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| AppError::io("read randomness for participant UUID", path, error))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
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
    let claude = env_utf8("CLAUDE_CODE_SESSION_ID")?;
    let thread = env_utf8("CODEX_THREAD_ID")?;
    let session = env_utf8("CODEX_SESSION_ID")?;
    let codex_present = thread.is_some() || session.is_some();
    match (claude, codex_present) {
        (Some(claude_key), true) => match nearest_native_harness() {
            Some(NativeHarness::Claude) => {
                return Ok(Some(native_binding(NativeHarness::Claude, claude_key)))
            }
            Some(NativeHarness::Codex) => {
                let key = codex_key(thread, session)?;
                return Ok(Some(native_binding(NativeHarness::Codex, key)));
            }
            None => {
                return Err(AppError::invalid_argument(
                    "both Claude and Codex conversation keys are set, but the nearest harness ancestor could not be determined; set POST_PARTICIPANT explicitly",
                )
                .reason("ambiguous nested harness ancestry"));
            }
        },
        (Some(key), false) => return Ok(Some(native_binding(NativeHarness::Claude, key))),
        (None, true) => {
            let key = codex_key(thread, session)?;
            return Ok(Some(native_binding(NativeHarness::Codex, key)));
        }
        (None, false) => {}
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
    let harness = env_utf8("POST_HARNESS")?.unwrap_or_else(|| harness.to_owned());
    Ok(Some(ConversationBinding {
        harness: harness_slug(&harness)?,
        key: key.to_owned(),
        provenance: Provenance::LauncherAddress,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeHarness {
    Claude,
    Codex,
}

fn native_binding(harness: NativeHarness, key: String) -> ConversationBinding {
    match harness {
        NativeHarness::Claude => ConversationBinding {
            harness: "claude".to_owned(),
            key,
            provenance: Provenance::HarnessClaude,
        },
        NativeHarness::Codex => ConversationBinding {
            harness: "codex".to_owned(),
            key,
            provenance: Provenance::HarnessCodex,
        },
    }
}

fn codex_key(thread: Option<String>, session: Option<String>) -> AppResult<String> {
    if let (Some(thread), Some(session)) = (&thread, &session) {
        if thread != session {
            return Err(AppError::invalid_argument(
                "CODEX_THREAD_ID and CODEX_SESSION_ID are both set but differ; refusing to guess the conversation key",
            )
            .reason("conflicting Codex conversation keys"));
        }
    }
    Ok(thread
        .or(session)
        .expect("at least one Codex key is present"))
}

fn nearest_native_harness() -> Option<NativeHarness> {
    let ancestors = process_ancestors()?;
    let claude_pid = std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    nearest_native_harness_in(
        ancestors
            .iter()
            .map(|(pid, command)| (*pid, command.as_str())),
        claude_pid,
    )
}

fn nearest_native_harness_in<'a>(
    ancestors: impl IntoIterator<Item = (u32, &'a str)>,
    claude_pid: Option<u32>,
) -> Option<NativeHarness> {
    for (pid, command) in ancestors {
        if claude_pid == Some(pid) {
            return Some(NativeHarness::Claude);
        }
        let basename = Path::new(command)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(command)
            .trim_start_matches('-');
        match basename {
            "claude" => return Some(NativeHarness::Claude),
            "codex" => return Some(NativeHarness::Codex),
            _ => {}
        }
    }
    None
}

fn process_ancestors() -> Option<Vec<(u32, String)>> {
    let (mut pid, _) = process_info(std::process::id())?;
    let mut ancestors = Vec::new();
    for _ in 0..16 {
        if pid == 0 {
            break;
        }
        let (parent, command) = process_info(pid)?;
        ancestors.push((pid, command));
        pid = parent;
    }
    Some(ancestors)
}

fn process_info(pid: u32) -> Option<(u32, String)> {
    let output = Command::new("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = std::str::from_utf8(&output.stdout).ok()?.trim();
    let split = line.find(char::is_whitespace)?;
    let parent = line[..split].trim().parse::<u32>().ok()?;
    let command = line[split..].trim();
    (!command.is_empty()).then(|| (parent, command.to_owned()))
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

#[cfg(test)]
thread_local! {
    static TEST_ACTORS: std::cell::RefCell<std::collections::BTreeMap<PathBuf, String>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

#[cfg(test)]
fn test_actor_id(context: &Context) -> Option<String> {
    TEST_ACTORS.with(|actors| actors.borrow().get(&context.root).cloned())
}

/// Unit-test support that seeds one durable participant record per fixture and
/// selects it without changing process-global environment. Product resolution
/// remains the only algorithm used after this explicit fixture binding.
#[cfg(test)]
pub(crate) fn bind_test_actor(context: &Context, workspace: &str) -> Participant {
    let key = format!("{}:{workspace}", context.root.display());
    let digest = digest(&key);
    let id = participant_id("test", &digest, 8);
    let dir = context.root.join(PARTICIPANTS_DIR).join(&id);
    let path = dir.join(RECORD_FILE);
    if !path.exists() {
        fs::create_dir_all(&dir).expect("create test participant directory");
        let participant = Participant {
            version: RECORD_VERSION,
            id: id.clone(),
            harness: "test".to_owned(),
            conversation_key_digest: digest,
            created: "2026-09-16 00:00:00 -0500".to_owned(),
            workspace: Some(workspace.to_owned()),
            workspace_path: None,
            lineage: None,
            lineage_since: None,
            display_name: None,
            dir: dir.clone(),
        };
        write_record(&participant).expect("write test participant record");
    }
    TEST_ACTORS.with(|actors| {
        actors.borrow_mut().insert(context.root.clone(), id.clone());
    });
    load(context, &id)
        .expect("load test participant")
        .expect("test participant exists")
}

#[cfg(test)]
mod tests {
    use super::{nearest_native_harness_from, nearest_native_harness_in, NativeHarness};

    #[test]
    fn nearest_harness_ancestor_selects_nested_codex_child() {
        let ancestors = [(40, "node"), (30, "/usr/local/bin/codex"), (20, "claude")];
        assert_eq!(
            nearest_native_harness_in(ancestors, None),
            Some(NativeHarness::Codex)
        );
    }

    #[test]
    fn claude_pid_marks_nearest_claude_ancestor_even_under_a_wrapper() {
        let ancestors = [(40, "node"), (30, "python"), (20, "codex")];
        assert_eq!(
            nearest_native_harness_in(ancestors, Some(30)),
            Some(NativeHarness::Claude)
        );
    }

    #[test]
    fn ambiguous_ancestor_list_never_guesses() {
        let ancestors = [(40, "node"), (30, "python"), (20, "bash")];
        assert_eq!(nearest_native_harness_in(ancestors, None), None);
    }

    #[test]
    fn recognized_nearest_harness_survives_unavailable_higher_ancestor() {
        for (command, expected) in [
            ("/usr/local/bin/codex", NativeHarness::Codex),
            ("/usr/local/bin/claude", NativeHarness::Claude),
        ] {
            let mut calls = Vec::new();
            let resolved = nearest_native_harness_from(100, None, |pid| {
                calls.push(pid);
                match pid {
                    100 => Some((90, "test-runner".to_owned())),
                    90 => Some((80, command.to_owned())),
                    _ => panic!("higher ancestor must not be read after recognizing {command}"),
                }
            });
            assert_eq!(resolved, Some(expected));
            assert_eq!(calls, vec![100, 90]);
        }
    }
}
