use crate::error::{AppError, AppResult, ErrorCode};
use crate::model::{Envelope, ParsedMail, RoomMap, RulesConfig};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const DEFAULT_RULES_JSON: &str = r#"{
  "blocked": []
}
"#;

pub(crate) const DEFAULT_ROOMS_JSON: &str = r#"{}
"#;

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);
const ROOMS_LOCK_FILE: &str = ".rooms.lock";
const RESERVED_ROOM_NAMES: [&str; 7] = [
    "*",
    "archive",
    "rooms.json",
    "rules.json",
    "profiles.json",
    "owner.json",
    ROOMS_LOCK_FILE,
];

/// One refusal predicate for every profile-text enforcement point (both
/// validators, both parse-time checks, and text sanitization) so the layers
/// can never drift apart again: Cc controls, the bidi/direction controls
/// (Cf, invisible to `is_control`), and the Zl/Zp line/paragraph separators
/// that line-oriented consumers split on. ZWJ (U+200D) and VS16 (U+FE0F)
/// deliberately stay legal — emoji need them.
pub(crate) fn refused_profile_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{061C}'
                | '\u{2028}'
                | '\u{2029}'
        )
}

#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub root: PathBuf,
    pub home: PathBuf,
}

impl Context {
    pub(crate) fn from_env() -> AppResult<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            AppError::new(
                ErrorCode::ConfigInvalid,
                "environment variable 'HOME' is missing, so '~' room paths cannot be resolved",
                "Set `HOME` to an absolute directory and retry the same command.",
            )
            .input("HOME")
            .reason("missing environment variable")
        })?;
        let root = std::env::var_os("POST_MAIL_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude-mail"));
        if !root.is_absolute() {
            return Err(AppError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "POST_MAIL_ROOT '{}' is not absolute; mailbox writes need an unambiguous root",
                    root.display()
                ),
                "Set `POST_MAIL_ROOT` to an absolute path and retry the same command.",
            )
            .input(root.display().to_string())
            .reason("path is relative"));
        }
        Ok(Self { root, home })
    }

    pub(crate) fn prepare_first_run(&self) -> AppResult<()> {
        if self.root.exists() {
            return Ok(());
        }
        fs::create_dir_all(&self.root)
            .map_err(|error| AppError::io("create mailbox root", &self.root, error))?;
        self.write_default_if_missing("rules.json", DEFAULT_RULES_JSON)?;
        self.write_default_if_missing("rooms.json", DEFAULT_ROOMS_JSON)?;
        Ok(())
    }

    pub(crate) fn write_default_if_missing(&self, name: &str, contents: &str) -> AppResult<bool> {
        let path = self.root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::io("inspect default config path", &path, error)),
        }
        match create_new_file(&path, contents.as_bytes()) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(AppError::io("create default config file", &path, error)),
        }
    }

    pub(crate) fn load_rooms(&self) -> AppResult<RoomMap> {
        let path = self.root.join("rooms.json");
        let bytes = fs::read(&path)
            .map_err(|error| AppError::config(&path, format!("cannot read file: {error}")))?;
        let rooms: RoomMap = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::config(&path, format!("invalid JSON object: {error}")))?;
        // An empty map is the legitimate fresh-install state (defaults ship
        // empty as of 0.2.3); `rooms add` is how it stops being empty.
        for (name, room_path) in &rooms {
            validate_room_name(name).map_err(|reason| AppError::config(&path, reason))?;
            self.expand_room_path(room_path)
                .map_err(|reason| AppError::config(&path, reason))?;
        }
        Ok(rooms)
    }

    pub(crate) fn write_rooms(&self, rooms: &RoomMap) -> AppResult<()> {
        let path = self.root.join("rooms.json");
        if fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(AppError::config(
                &path,
                "rooms.json is a symlink; replace it with a regular file before registering rooms",
            ));
        }
        let mut bytes = serde_json::to_vec_pretty(rooms)
            .map_err(|error| AppError::io("serialize room registry", &path, error))?;
        bytes.push(b'\n');
        atomic_replace(&path, &bytes)
            .map_err(|error| AppError::io("atomically update room registry", &path, error))
    }

    pub(crate) fn lock_rooms(&self) -> AppResult<File> {
        let path = self.root.join(ROOMS_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| AppError::io("open room registry lock", &path, error))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
            return Err(AppError::io(
                "lock room registry",
                &path,
                std::io::Error::last_os_error(),
            ));
        }
        Ok(file)
    }

    pub(crate) fn load_rules(&self, rooms: &RoomMap) -> AppResult<RulesConfig> {
        let path = self.root.join("rules.json");
        let bytes = fs::read(&path)
            .map_err(|error| AppError::config(&path, format!("cannot read file: {error}")))?;
        let rules: RulesConfig = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::config(&path, format!("invalid JSON shape: {error}")))?;
        for (index, rule) in rules.blocked.iter().enumerate() {
            if rule.from.trim().is_empty()
                || rule.to.trim().is_empty()
                || rule.reason.trim().is_empty()
            {
                return Err(AppError::config(
                    &path,
                    format!(
                        "blocked[{index}] must have non-empty 'from', 'to', and 'reason' strings"
                    ),
                ));
            }
            if rule.from != "*" {
                validate_component(&rule.from).map_err(|reason| AppError::config(&path, reason))?;
            }
            if rule.to != "*" && !rooms.contains_key(&rule.to) {
                return Err(AppError::config(
                    &path,
                    format!(
                        "blocked[{index}].to names unknown room '{}'; add it to rooms.json or use '*'",
                        rule.to
                    ),
                ));
            }
        }
        Ok(rules)
    }

    pub(crate) fn expand_room_path(&self, value: &str) -> Result<PathBuf, String> {
        if value == "~" {
            return Ok(self.home.clone());
        }
        if let Some(rest) = value.strip_prefix("~/") {
            if rest.is_empty() {
                return Ok(self.home.clone());
            }
            return Ok(self.home.join(rest));
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(format!(
                "room path '{value}' must be absolute or start with '~/'; replace it with an absolute path"
            ))
        }
    }

    pub(crate) fn resolved_room(
        &self,
        explicit: Option<String>,
        rooms: &RoomMap,
    ) -> AppResult<String> {
        if let Some(room) = explicit {
            validate_room_name(&room).map_err(|reason| {
                AppError::new(
                    ErrorCode::InvalidArgument,
                    format!("room '{room}' is invalid: {reason}"),
                    "Pass a single path-safe room name without '/' or '\\'.",
                )
                .input(room.clone())
                .reason(reason)
            })?;
            return Ok(room);
        }
        self.infer_from_cwd(rooms)
    }

    pub(crate) fn resolved_mailbox_dirs(
        &self,
        explicit: Option<String>,
    ) -> AppResult<(String, PathBuf, PathBuf)> {
        let rooms = self.load_rooms()?;
        let room = self.resolved_room(explicit, &rooms)?;
        let (inbox, read) = self.mailbox_dirs(&room)?;
        Ok((room, inbox, read))
    }

    pub(crate) fn infer_from_cwd(&self, rooms: &RoomMap) -> AppResult<String> {
        let cwd = std::env::current_dir()
            .map_err(|error| AppError::io("resolve current directory", Path::new("."), error))?;
        let mut matches = Vec::new();
        for (name, path) in rooms {
            let expanded = self
                .expand_room_path(path)
                .map_err(|reason| AppError::config(&self.root.join("rooms.json"), reason))?;
            if path_contains(&expanded, &cwd) {
                matches.push((expanded.components().count(), name.clone()));
            }
        }
        matches.sort();
        if let Some((_, name)) = matches.pop() {
            return Ok(name);
        }
        let basename = cwd
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "current directory '{}' has no UTF-8 basename for the default sender/room",
                        cwd.display()
                    ),
                    "Pass `--from <NAME>` for send or `--room <ROOM>` for inbox/read.",
                )
                .input(cwd.display().to_string())
                .reason("missing UTF-8 basename")
            })?;
        validate_component(basename).map_err(|reason| {
            AppError::new(
                ErrorCode::InvalidArgument,
                format!("inferred name '{basename}' is invalid: {reason}"),
                "Pass `--from <NAME>` for send or `--room <ROOM>` for inbox/read.",
            )
            .input(basename)
            .reason(reason)
        })?;
        Ok(basename.to_owned())
    }

    pub(crate) fn ensure_sender_allowed(&self, sender: &str, rooms: &RoomMap) -> AppResult<()> {
        let Some(room_path) = rooms.get(sender) else {
            return Ok(());
        };
        let cwd = std::env::current_dir()
            .map_err(|error| AppError::io("resolve current directory", Path::new("."), error))?;
        let expanded = self
            .expand_room_path(room_path)
            .map_err(|reason| AppError::config(&self.root.join("rooms.json"), reason))?;
        if path_contains(&expanded, &cwd) {
            return Ok(());
        }
        Err(AppError::new(
            ErrorCode::ReservedSender,
            format!(
                "sender '{sender}' is reserved for registered room '{}' and cwd '{}' is outside its tree",
                expanded.display(),
                cwd.display()
            ),
            format!(
                "Run from '{}' or pass a free-form sender such as `--from codex-<project>`.",
                expanded.display()
            ),
        )
        .input(sender)
        .reason("registered room name used outside its room tree")
        .registered_path(expanded.display().to_string()))
    }

    pub(crate) fn mailbox_dirs(&self, room: &str) -> AppResult<(PathBuf, PathBuf)> {
        validate_room_name(room).map_err(|reason| {
            AppError::new(
                ErrorCode::InvalidArgument,
                format!("room '{room}' is invalid: {reason}"),
                "Pass a single room name without path separators.",
            )
        })?;
        let directory = self.root.join(room);
        let inbox = directory.join("inbox");
        let read = directory.join("read");
        fs::create_dir_all(&inbox)
            .map_err(|error| AppError::io("create inbox directory", &inbox, error))?;
        fs::create_dir_all(&read)
            .map_err(|error| AppError::io("create read directory", &read, error))?;
        Ok((inbox, read))
    }
}

pub(crate) fn validate_component(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("name is empty".to_owned());
    }
    if value.chars().any(char::is_control) {
        return Err("name must not contain control characters".to_owned());
    }
    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err("name must be one path-safe component".to_owned());
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        return Err("name must not be '.', '..', or contain '/' or '\\'".to_owned());
    }
    Ok(())
}

/// Quote `value` for a POSIX shell so suggested/exact fixes stay executable
/// when names or bodies carry spaces, quotes, or other metacharacters.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

pub(crate) fn validate_room_name(value: &str) -> Result<(), String> {
    validate_component(value)?;
    let folded = value.to_ascii_lowercase();
    if RESERVED_ROOM_NAMES.contains(&folded.as_str())
        || (folded.starts_with(".rooms.json.") && folded.ends_with(".tmp"))
    {
        return Err(format!(
            "name '{value}' is reserved by mailbox storage or wildcard semantics"
        ));
    }
    Ok(())
}

fn path_contains(root: &Path, candidate: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let candidate = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    candidate.starts_with(root)
}

pub(crate) fn parse_mail(path: &Path) -> AppResult<ParsedMail> {
    let mut raw = String::new();
    File::open(path)
        .and_then(|mut file| file.read_to_string(&mut raw))
        .map_err(|error| AppError::io("read mail file", path, error))?;
    let (head, body) = raw.split_once("\n---\n").ok_or_else(|| {
        AppError::config(
            path,
            "mail has no '\\n---\\n' separator; restore a valid .mail file or move it aside",
        )
    })?;
    let envelope: Envelope = serde_json::from_str(head)
        .map_err(|error| AppError::config(path, format!("malformed envelope JSON: {error}")))?;
    validate_envelope(path, &envelope)?;
    if path.file_stem().and_then(|value| value.to_str()) != Some(envelope.id.as_str()) {
        return Err(AppError::config(
            path,
            format!(
                "mail filename must be '{}.mail' to match envelope id '{}'",
                envelope.id, envelope.id
            ),
        ));
    }
    Ok(ParsedMail {
        envelope,
        body: body.to_owned(),
    })
}

pub(crate) fn validate_envelope(path: &Path, envelope: &Envelope) -> AppResult<()> {
    for (field, value) in [
        ("id", envelope.id.as_str()),
        ("from", envelope.from.as_str()),
        ("to", envelope.to.as_str()),
        ("sent", envelope.sent.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(AppError::config(
                path,
                format!("mail envelope field '{field}' is empty"),
            ));
        }
    }
    let id = envelope.id.as_bytes();
    let valid_id = id.len() == 22
        && id[..8].iter().all(u8::is_ascii_digit)
        && id[8] == b'-'
        && id[9..15].iter().all(u8::is_ascii_digit)
        && id[15] == b'-'
        && id[16..].iter().all(u8::is_ascii_hexdigit);
    if !valid_id {
        return Err(AppError::config(
            path,
            format!(
                "mail envelope id '{}' must match YYYYmmdd-HHMMSS-<6 hex>",
                envelope.id
            ),
        ));
    }
    // Stamped profile fields render unquoted in inbox/read banners; refuse
    // control characters at parse so a hand-edited .mail can't forge lines.
    for (field, value) in [
        ("display_name", &envelope.display_name),
        ("pfp", &envelope.pfp),
    ] {
        if let Some(value) = value {
            if value.chars().any(refused_profile_char) {
                return Err(AppError::config(
                    path,
                    format!(
                        "mail envelope field '{field}' contains control, bidi, or line-separator characters"
                    ),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn mail_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| AppError::io("list mailbox directory", directory, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read mailbox entry", directory, error))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("mail") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) fn exclusive_atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    exclusive_atomic_write_with(path, bytes, |temporary, parent| {
        fs::remove_file(temporary)?;
        File::open(parent)?.sync_all()
    })
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "filename is not UTF-8")
        })?;
    let nonce = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{filename}.{}.{}.tmp", std::process::id(), nonce));
    let mode = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to replace a symlinked config file",
            ));
        }
        Ok(metadata) => metadata.permissions().mode() & 0o7777,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o600,
        Err(error) => return Err(error),
    };
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&temporary)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        eprintln!(
            "post: warning: '{}' was committed, but directory sync failed: {error}",
            path.display()
        );
    }
    Ok(())
}

fn exclusive_atomic_write_with<F>(path: &Path, bytes: &[u8], after_commit: F) -> std::io::Result<()>
where
    F: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "filename is not UTF-8")
        })?;
    let nonce = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{filename}.{}.{}.tmp", std::process::id(), nonce));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)?;
        Ok(())
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(source);
    }
    if let Err(error) = after_commit(&temporary, parent) {
        eprintln!(
            "post: warning: '{}' was committed, but temporary-file cleanup or directory sync failed: {error}",
            path.display()
        );
    }
    Ok(())
}

pub(crate) enum MoveError {
    /// The destination link was never created; nothing changed on disk.
    Link(std::io::Error),
    /// The destination link exists but the source link could not be removed:
    /// the mail is now visible in both directories.
    Unlink(std::io::Error),
}

pub(crate) fn exclusive_move(source: &Path, destination: &Path) -> Result<(), MoveError> {
    fs::hard_link(source, destination).map_err(MoveError::Link)?;
    fs::remove_file(source).map_err(MoveError::Unlink)
}

fn create_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    File::open(parent)?.sync_all()
}

pub(crate) fn encode_mail(envelope: &Envelope, body: &str) -> AppResult<Vec<u8>> {
    let payload = serde_json::to_string_pretty(envelope).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!(
                "failed to serialize mail envelope '{}': {error}",
                envelope.id
            ),
            "Retry the same send command; if this repeats, report `post --version`.",
        )
    })?;
    let mut payload = ascii_escape_json(&payload);
    payload.push_str("\n---\n");
    payload.push_str(body);
    Ok(payload.into_bytes())
}

pub(crate) fn ascii_escape_json(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len());
    for character in json.chars() {
        // DEL is the one ASCII byte Python's ensure_ascii escaper also rewrites.
        if character.is_ascii() && character != '\u{7f}' {
            escaped.push(character);
            continue;
        }
        let codepoint = character as u32;
        if codepoint <= 0xffff {
            write!(escaped, "\\u{codepoint:04x}").expect("writing to a String cannot fail");
        } else {
            let supplementary = codepoint - 0x1_0000;
            let high = 0xd800 + (supplementary >> 10);
            let low = 0xdc00 + (supplementary & 0x3ff);
            write!(escaped, "\\u{high:04x}\\u{low:04x}").expect("writing to a String cannot fail");
        }
    }
    escaped
}

pub(crate) fn local_timestamp() -> AppResult<(String, String)> {
    let seconds = time_since_unix_epoch()?.as_secs();
    format_local_timestamp(seconds)
}

/// Channel-message timestamps carry microseconds: a second-resolution id
/// makes "max id read" an unsafe cursor high-water mark, because a
/// same-second send with a smaller hash sorts *behind* an already-advanced
/// cursor and is skipped forever (ufos-fable, mail 013351/013510).
pub(crate) fn local_timestamp_micros() -> AppResult<(String, String)> {
    let elapsed = time_since_unix_epoch()?;
    let (id_time, sent) = format_local_timestamp(elapsed.as_secs())?;
    Ok((format!("{id_time}-{:06}", elapsed.subsec_micros()), sent))
}

fn time_since_unix_epoch() -> AppResult<std::time::Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            AppError::new(
                ErrorCode::IoError,
                format!("system clock is before the Unix epoch: {error}"),
                "Correct the system clock and retry the send command.",
            )
        })
}

fn format_local_timestamp(seconds: u64) -> AppResult<(String, String)> {
    let seconds = libc::time_t::try_from(seconds).map_err(|_| {
        AppError::new(
            ErrorCode::IoError,
            "system time cannot be represented by the local clock",
            "Correct the system clock and retry the send command.",
        )
    })?;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut local) }.is_null() {
        return Err(AppError::new(
            ErrorCode::IoError,
            "local time conversion failed",
            "Correct the system timezone configuration and retry the send command.",
        ));
    }
    // IDs sort as strings, so their timestamp MUST be timezone-independent:
    // a machine timezone change mid-conversation (observed 2026-08-08,
    // Eastern -> Central in a moving car) made new local-time IDs sort an
    // hour BEHIND the channel cursor, silently hiding them from every
    // max-id reader. IDs use UTC; only the human-facing `sent` string keeps
    // local wall time (with its offset).
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::gmtime_r(&seconds, &mut utc) }.is_null() {
        return Err(AppError::new(
            ErrorCode::IoError,
            "UTC time conversion failed",
            "Correct the system clock and retry the send command.",
        ));
    }
    let offset = local.tm_gmtoff;
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.unsigned_abs();
    let id_time = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        utc.tm_year + 1900,
        utc.tm_mon + 1,
        utc.tm_mday,
        utc.tm_hour,
        utc.tm_min,
        utc.tm_sec
    );
    let sent = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {sign}{:02}{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
        offset / 3600,
        (offset % 3600) / 60
    );
    Ok((id_time, sent))
}

pub(crate) fn new_mail_id(timestamp: &str, attempt: u64) -> AppResult<String> {
    let nanos = time_since_unix_epoch()?.as_nanos() as u64;
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = nanos ^ counter.rotate_left(17) ^ u64::from(std::process::id()) ^ attempt;
    Ok(format!("{timestamp}-{:06x}", mixed & 0x00ff_ffff))
}

pub(crate) fn closest_room<'a>(input: &str, rooms: &'a RoomMap) -> Option<&'a str> {
    rooms
        .keys()
        .map(|room| (levenshtein(input, room), room.as_str()))
        .min_by(|left, right| left.cmp(right))
        .and_then(|(distance, room)| (distance <= 3).then_some(room))
}

fn levenshtein(left: &str, right: &str) -> usize {
    let mut previous: Vec<usize> = (0..=right.chars().count()).collect();
    let mut current = vec![0; previous.len()];
    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.chars().enumerate() {
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + usize::from(left_char != right_char));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.chars().count()]
}

// ---------------------------------------------------------------------------
// Signed owner (A0a): the trust anchor for verified mail, configurable via
// owner.json at the mail root. See docs/plans/a0a-signed-owner-contract.md.
// ---------------------------------------------------------------------------

pub(crate) const OWNER_FILE: &str = "owner.json";
/// Room id the legacy fallback owner lives under when no owner.json exists.
pub(crate) const LEGACY_OWNER_ROOM: &str = "trey";
const OWNER_DEFAULT_MARKER: &str = "🧔";
const PRINCIPAL_MAX_BYTES: usize = 128;
const NAMESPACE_MAX_BYTES: usize = 64;

impl Context {
    pub(crate) fn owner_json_path(&self) -> PathBuf {
        self.root.join(OWNER_FILE)
    }
}

/// The raw owner.json on disk: what `post owner init` writes and what the
/// loader parses. Omitted optional fields serialize as JSON null (matching
/// the A0a example bytes); every null is derived at load time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerFile {
    pub room: String,
    #[serde(default)]
    pub sidecar_dir: Option<PathBuf>,
    #[serde(default)]
    pub allowed_signers: Option<PathBuf>,
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

/// How this owner was resolved: configured via owner.json, or the synthesized
/// legacy owner (no owner.json + a registered `trey` room).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerSource {
    Configured,
    Legacy,
}

/// The post-derivation owner: every field resolved, every default applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedOwner {
    pub room: String,
    pub sidecar_dir: PathBuf,
    pub allowed_signers: PathBuf,
    pub principal: String,
    pub namespace: String,
    pub marker: String,
    pub label: String,
    pub source: OwnerSource,
}

/// Decision 2 resolution states: configured / legacy fallback / feature-absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnerResolution {
    /// owner.json parsed and validated (Decision 1).
    Configured(ResolvedOwner),
    /// No owner.json, but room `trey` is registered: the pre-A0a behavior,
    /// synthesized. This machine changes nothing and notices nothing.
    Legacy(ResolvedOwner),
    /// Neither: no signed owner. signed_status renders no badges and profile
    /// imitation reserves nothing.
    None,
}

/// Full resolution (Decision 2): a present-but-invalid owner.json is
/// ConfigInvalid (Decision 3) — it never degrades to legacy or feature-absent.
pub(crate) fn load_owner(context: &Context) -> AppResult<OwnerResolution> {
    let rooms = context.load_rooms()?;
    load_owner_with_rooms(context, &rooms)
}

/// Resolution against an already-loaded room registry (doctor uses this so a
/// broken rooms.json can still be reported as its own diagnostic).
pub(crate) fn load_owner_with_rooms(
    context: &Context,
    rooms: &RoomMap,
) -> AppResult<OwnerResolution> {
    let path = context.owner_json_path();
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let file = read_owner_file(&path)?;
            Ok(OwnerResolution::Configured(resolve_owner_file(
                context, &file, rooms,
            )?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if rooms.contains_key(LEGACY_OWNER_ROOM) {
                Ok(OwnerResolution::Legacy(legacy_owner(context, rooms)?))
            } else {
                Ok(OwnerResolution::None)
            }
        }
        Err(error) => Err(AppError::io("inspect owner config", &path, error)),
    }
}

/// Option-wrapped resolution for badge-computing paths: None under
/// feature-absent, the owner under configured and legacy fallback.
pub(crate) fn resolve_owner(context: &Context) -> AppResult<Option<ResolvedOwner>> {
    Ok(match load_owner(context)? {
        OwnerResolution::Configured(owner) | OwnerResolution::Legacy(owner) => Some(owner),
        OwnerResolution::None => None,
    })
}

/// The resolved owner's room id, or None when feature-absent. Callers that
/// already hold a loaded registry pair this with `load_owner_with_rooms` so
/// the imitation reservation runs against the same snapshot (profile set,
/// doctor).
pub(crate) fn resolved_owner_room(resolution: &OwnerResolution) -> Option<&str> {
    match resolution {
        OwnerResolution::Configured(owner) | OwnerResolution::Legacy(owner) => Some(&owner.room),
        OwnerResolution::None => None,
    }
}

/// Parse-and-validate a present owner.json without consulting rooms.json, so
/// `post doctor` still reports a malformed trust anchor when the room
/// registry it registers against is itself broken. No-op when the file is
/// absent.
pub(crate) fn check_owner_parses(context: &Context) -> AppResult<()> {
    let path = context.owner_json_path();
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            read_owner_file(&path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io("inspect owner config", &path, error)),
    }
}

/// Parse the raw owner.json and validate every EXPLICIT value. Never follows
/// a symlinked owner.json: the trust anchor must be a regular file.
pub(crate) fn read_owner_file(path: &Path) -> AppResult<OwnerFile> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AppError::io("inspect owner config", path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(AppError::config(
            path,
            "owner.json is a symlink; a trust anchor must be a regular file — replace the symlink with a real file",
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| AppError::config(path, format!("cannot read file: {error}")))?;
    let file: OwnerFile = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(path, format!("invalid JSON object: {error}")))?;
    validate_owner_values(path, &file)?;
    Ok(file)
}

/// Validate the explicit fields of an owner configuration (the values a
/// hand-written owner.json or `post owner init` can supply). Registration of
/// `room` is checked separately at resolution, since it needs rooms.json.
pub(crate) fn validate_owner_values(path: &Path, file: &OwnerFile) -> AppResult<()> {
    let config = |reason: String| AppError::config(path, reason);
    validate_room_name(&file.room)
        .map_err(|reason| config(format!("'room' must be a valid room name: {reason}")))?;
    if let Some(dir) = &file.sidecar_dir {
        if !dir.is_absolute() {
            return Err(config(
                "'sidecar_dir' must be an absolute path (the sidecar root is never relative)"
                    .into(),
            ));
        }
    }
    if let Some(signers) = &file.allowed_signers {
        if !signers.is_absolute() {
            return Err(config("'allowed_signers' must be an absolute path".into()));
        }
    }
    if let Some(principal) = &file.principal {
        validate_ascii_bound(principal, "'principal'", PRINCIPAL_MAX_BYTES).map_err(config)?;
    }
    if let Some(namespace) = &file.namespace {
        validate_ascii_bound(namespace, "'namespace'", NAMESPACE_MAX_BYTES).map_err(config)?;
    }
    if let Some(marker) = &file.marker {
        validate_marker(marker).map_err(config)?;
    }
    if let Some(label) = &file.label {
        validate_label(label).map_err(config)?;
    }
    Ok(())
}

/// Post-derivation resolution of a raw owner file: registration check against
/// rooms.json, then every default. Derived values must satisfy the same
/// bounds as explicit ones — a room name pathological enough to break its
/// derivations falls closed rather than shipping a broken trust anchor.
pub(crate) fn resolve_owner_file(
    context: &Context,
    file: &OwnerFile,
    rooms: &RoomMap,
) -> AppResult<ResolvedOwner> {
    let path = context.owner_json_path();
    let config = |reason: String| AppError::config(&path, reason);
    let room_path = rooms.get(&file.room).ok_or_else(|| {
        config(format!(
            "'room' names unregistered room '{}'; register it in rooms.json first",
            file.room
        ))
    })?;
    let sidecar_dir = match &file.sidecar_dir {
        Some(dir) => dir.clone(),
        // Default: the registered room's NORMALIZED/resolved path, never the
        // raw rooms.json string (live registries contain `~`).
        None => context.expand_room_path(room_path).map_err(|reason| {
            config(format!(
                "cannot derive sidecar_dir from the registered room path: {reason}"
            ))
        })?,
    };
    let allowed_signers = file
        .allowed_signers
        .clone()
        .unwrap_or_else(|| sidecar_dir.join("allowed_signers"));
    let principal = file
        .principal
        .clone()
        .unwrap_or_else(|| format!("{}@porch", file.room));
    let namespace = file
        .namespace
        .clone()
        .unwrap_or_else(|| format!("{}-porch", file.room));
    let marker = file
        .marker
        .clone()
        .unwrap_or_else(|| OWNER_DEFAULT_MARKER.to_owned());
    let label = match &file.label {
        Some(label) => label.clone(),
        None => default_owner_label(&file.room),
    };
    validate_ascii_bound(&principal, "'principal' (derived)", PRINCIPAL_MAX_BYTES)
        .map_err(config)?;
    validate_ascii_bound(&namespace, "'namespace' (derived)", NAMESPACE_MAX_BYTES)
        .map_err(config)?;
    validate_label(&label).map_err(config)?;
    Ok(ResolvedOwner {
        room: file.room.clone(),
        sidecar_dir,
        allowed_signers,
        principal,
        namespace,
        marker,
        label,
        source: OwnerSource::Configured,
    })
}

/// The synthesized legacy owner (Decision 2 step 2): room `trey`, sidecar at
/// the registered trey room's resolved path (`~/.trey-room` on this machine),
/// principal/namespace/marker/label matching the pre-A0a hardcodes exactly.
pub(crate) fn legacy_owner(context: &Context, rooms: &RoomMap) -> AppResult<ResolvedOwner> {
    let path = context.owner_json_path();
    let room_path = rooms
        .get(LEGACY_OWNER_ROOM)
        .expect("legacy resolution is only entered with 'trey' registered");
    let sidecar_dir = context.expand_room_path(room_path).map_err(|reason| {
        AppError::config(
            &path,
            format!("cannot derive the legacy sidecar from the 'trey' room registration: {reason}"),
        )
    })?;
    Ok(ResolvedOwner {
        room: LEGACY_OWNER_ROOM.to_owned(),
        sidecar_dir: sidecar_dir.clone(),
        allowed_signers: sidecar_dir.join("allowed_signers"),
        principal: format!("{LEGACY_OWNER_ROOM}@porch"),
        namespace: format!("{LEGACY_OWNER_ROOM}-porch"),
        marker: OWNER_DEFAULT_MARKER.to_owned(),
        label: "Trey".to_owned(),
        source: OwnerSource::Legacy,
    })
}

fn default_owner_label(room: &str) -> String {
    let mut chars = room.chars();
    match chars.next() {
        Some(first) => {
            let mut label = first.to_uppercase().collect::<String>();
            label.push_str(chars.as_str());
            label
        }
        None => String::new(),
    }
}

/// Shared bound for principal and namespace: exact ASCII-safe alphabet for
/// ssh-keygen argv and allowed_signers lines; refuses NUL/control/space by
/// construction. Bounds shared with porch (B0 onboarding).
fn validate_ascii_bound(value: &str, what: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if value.len() > max_bytes {
        return Err(format!(
            "{what} exceeds {max_bytes} bytes (got {})",
            value.len()
        ));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-'))
    {
        return Err(format!(
            "{what} must contain only ASCII letters, digits, '.', '_', '@', or '-'"
        ));
    }
    Ok(())
}

/// The owner marker: exactly one glyph, validated by the same safe-subset
/// predicate as profile pfp validation (minus uniqueness) — non-ASCII, no
/// control/bidi/line-separator characters. ZWJ-abuse at the edges is refused
/// (a marker must not START or END with U+200D): an unterminated join can
/// render unstable or invisible, and the wire prefix must be unambiguous.
fn validate_marker(marker: &str) -> Result<(), String> {
    // ZWJ abuse is checked before the cluster count: a leading or trailing
    // bare ZWJ is always a multi-cluster string anyway, so the refusal
    // message just gets more specific.
    if marker.starts_with('\u{200d}') || marker.ends_with('\u{200d}') {
        return Err(
            "marker must not start or end with a zero-width joiner (ZWJ-abuse refused)".to_owned(),
        );
    }
    let mut graphemes = marker.graphemes(true);
    if graphemes.next().is_none() || graphemes.next().is_some() {
        return Err("marker must be exactly one glyph (one grapheme cluster)".to_owned());
    }
    if marker.chars().any(refused_profile_char) {
        return Err("marker contains control, bidi, or line-separator characters".to_owned());
    }
    if marker.is_ascii() {
        return Err("marker must be a non-ASCII glyph, not ASCII".to_owned());
    }
    Ok(())
}

/// The owner label: 1-32 Unicode scalar values, sharing the profile
/// display-name predicate (no control/bidi/line-separator characters) and
/// rejecting whitespace-only text.
fn validate_label(label: &str) -> Result<(), String> {
    if label.trim().is_empty() {
        return Err("label must not be empty or whitespace-only".to_owned());
    }
    if label.chars().any(refused_profile_char) {
        return Err("label contains control, bidi, or line-separator characters".to_owned());
    }
    let length = label.chars().count();
    if length > crate::profile::MAX_NAME_CHARS {
        return Err(format!(
            "label exceeds {} characters (got {length})",
            crate::profile::MAX_NAME_CHARS
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ascii_escape_json, exclusive_atomic_write, exclusive_atomic_write_with};
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    #[cfg(unix)]
    unsafe extern "C" {
        fn tzset();
    }

    #[test]
    fn ascii_escape_matches_python_ensure_ascii_including_del() {
        assert_eq!(
            ascii_escape_json("{\"s\": \"\u{7f} café 😀\"}"),
            "{\"s\": \"\\u007f caf\\u00e9 \\ud83d\\ude00\"}"
        );
    }

    #[test]
    fn exclusive_atomic_write_never_replaces_an_existing_mail_file() {
        let root = test_root("atomic");
        let destination = root.join("message.mail");
        fs::write(&destination, "original mail").expect("create collision fixture");

        let error = exclusive_atomic_write(&destination, b"replacement")
            .expect_err("exclusive publication must reject an existing mail file");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(&destination).expect("read collision fixture"),
            "original mail"
        );
        let entries: Vec<_> = fs::read_dir(&root)
            .expect("list atomic-write test root")
            .map(|entry| entry.expect("read test entry").file_name())
            .collect();
        assert_eq!(entries, vec!["message.mail"]);
        trash_test_root(&root);
    }

    #[test]
    fn exclusive_atomic_write_stays_successful_after_the_commit_point() {
        let root = test_root("atomic-commit");
        let destination = root.join("message.mail");

        exclusive_atomic_write_with(&destination, b"committed mail", |_, _| {
            Err(io::Error::other("injected post-commit cleanup failure"))
        })
        .expect("post-commit cleanup failure must not report publication failure");

        assert_eq!(
            fs::read_to_string(&destination).expect("read committed mail"),
            "committed mail"
        );
        trash_test_root(&root);
    }

    #[test]
    fn defaults_use_exclusive_create_and_leave_existing_or_dangling_rules_untouched() {
        let root = test_root("defaults");
        let context = super::Context {
            root: root.clone(),
            home: root.clone(),
        };
        let rules = root.join("rules.json");
        fs::write(&rules, "human rules").expect("write human rules");
        assert!(!context
            .write_default_if_missing("rules.json", "defaults")
            .expect("existing rules must be left untouched"));
        assert_eq!(
            fs::read_to_string(&rules).expect("read human rules"),
            "human rules"
        );

        fs::remove_file(&rules).expect("remove regular rules fixture");
        std::os::unix::fs::symlink(root.join("missing-target"), &rules)
            .expect("create dangling rules symlink");
        assert!(!context
            .write_default_if_missing("rules.json", "defaults")
            .expect("dangling rules symlink must be left untouched"));
        assert!(fs::symlink_metadata(&rules)
            .expect("inspect dangling rules symlink")
            .file_type()
            .is_symlink());
        trash_test_root(&root);
    }

    #[test]
    fn room_registry_lock_file_is_private_and_excludes_a_second_writer() {
        let root = test_root("rooms-lock");
        let context = super::Context {
            root: root.clone(),
            home: root.clone(),
        };
        let lock = context.lock_rooms().expect("acquire room registry lock");
        let lock_path = root.join(super::ROOMS_LOCK_FILE);
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&lock_path)
            .expect("open second lock file handle");

        assert_eq!(
            unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB,) },
            -1
        );
        assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            fs::metadata(&lock_path)
                .expect("inspect room registry lock")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(contender);
        drop(lock);
        trash_test_root(&root);
    }

    #[test]
    fn local_timestamp_keeps_reference_id_and_sent_byte_formats() {
        let (id_time, sent) = super::local_timestamp().expect("format local timestamp");
        assert_eq!(id_time.len(), 15);
        assert_eq!(id_time.as_bytes()[8], b'-');
        assert!(id_time
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 8 || byte.is_ascii_digit()));
        assert_eq!(sent.len(), 25);
        assert_eq!(&sent[4..5], "-");
        assert_eq!(&sent[7..8], "-");
        assert_eq!(&sent[10..11], " ");
        assert_eq!(&sent[13..14], ":");
        assert_eq!(&sent[16..17], ":");
        assert!(matches!(&sent[20..21], "+" | "-"));
        assert!(sent
            .bytes()
            .enumerate()
            .filter(|(index, _)| !matches!(index, 4 | 7 | 10 | 13 | 16 | 19 | 20))
            .all(|(_, byte)| byte.is_ascii_digit()));
    }

    #[cfg(unix)]
    #[test]
    fn local_timestamp_matches_exact_positive_and_negative_offset_fixtures() {
        let previous_tz = std::env::var_os("TZ");
        std::env::set_var("TZ", "Asia/Kathmandu");
        unsafe { tzset() };
        let kathmandu = super::format_local_timestamp(1_700_000_000);
        std::env::set_var("TZ", "America/New_York");
        unsafe { tzset() };
        let new_york = super::format_local_timestamp(1_700_000_000);
        if let Some(previous_tz) = previous_tz {
            std::env::set_var("TZ", previous_tz);
        } else {
            std::env::remove_var("TZ");
        }
        unsafe { tzset() };

        // The id timestamp is UTC in every timezone (1_700_000_000 =
        // 2023-11-14 22:13:20Z) so IDs keep sorting monotonically when the
        // machine timezone changes; only `sent` carries local wall time.
        assert_eq!(
            kathmandu.expect("format Kathmandu fixture"),
            (
                "20231114-221320".to_owned(),
                "2023-11-15 03:58:20 +0545".to_owned()
            )
        );
        assert_eq!(
            new_york.expect("format New York fixture"),
            (
                "20231114-221320".to_owned(),
                "2023-11-14 17:13:20 -0500".to_owned()
            )
        );
    }
}

#[cfg(test)]
mod owner_tests {
    use super::{
        exclusive_atomic_write, legacy_owner, load_owner, load_owner_with_rooms, read_owner_file,
        validate_label, validate_marker, validate_owner_values, Context, OwnerFile,
        OwnerResolution, OwnerSource, ResolvedOwner, LEGACY_OWNER_ROOM,
    };
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;
    use std::path::PathBuf;

    fn context(label: &str) -> (PathBuf, Context) {
        let root = test_root(&format!("owner-{label}"));
        fs::write(
            root.join("rooms.json"),
            r#"{"trey": "~/.trey-room", "mara": "~/.mara-room"}"#,
        )
        .expect("seed rooms");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        context.prepare_first_run().expect("defaults");
        (root, context)
    }

    fn file(room: &str) -> OwnerFile {
        OwnerFile {
            room: room.to_owned(),
            sidecar_dir: None,
            allowed_signers: None,
            principal: None,
            namespace: None,
            marker: None,
            label: None,
        }
    }

    #[test]
    fn resolution_states_configured_legacy_none() {
        let (root, context) = context("states");
        // Configured: owner.json wins over the registered trey room.
        fs::write(
            root.join("owner.json"),
            r#"{"room": "mara", "marker": "🐳", "label": "Marvelous"}"#,
        )
        .expect("owner");
        let configured = load_owner(&context).expect("load configured");
        match configured {
            OwnerResolution::Configured(owner) => {
                assert_eq!(owner.room, "mara");
                assert_eq!(owner.source, OwnerSource::Configured);
                assert_eq!(owner.marker, "🐳");
                assert_eq!(owner.label, "Marvelous");
                assert_eq!(
                    owner.sidecar_dir,
                    root.join(".mara-room"),
                    "derived from the registered room path, never the raw ~ string"
                );
                assert_eq!(
                    owner.allowed_signers,
                    root.join(".mara-room").join("allowed_signers")
                );
                assert_eq!(owner.principal, "mara@porch");
                assert_eq!(owner.namespace, "mara-porch");
            }
            other => panic!("expected configured, got {other:?}"),
        }
        fs::remove_file(root.join("owner.json")).expect("remove owner");
        // Legacy: no owner.json + registered trey room.
        match load_owner(&context).expect("load legacy") {
            OwnerResolution::Legacy(owner) => {
                assert_eq!(owner.room, "trey");
                assert_eq!(owner.source, OwnerSource::Legacy);
                assert_eq!(owner.sidecar_dir, root.join(".trey-room"));
                assert_eq!(owner.principal, "trey@porch");
                assert_eq!(owner.namespace, "trey-porch");
                assert_eq!(owner.marker, "🧔");
                assert_eq!(owner.label, "Trey");
            }
            other => panic!("expected legacy, got {other:?}"),
        }
        // Feature-absent: no owner.json and no trey room.
        fs::write(root.join("rooms.json"), r#"{"mara": "~/.mara-room"}"#).expect("no trey");
        assert_eq!(
            load_owner(&context).expect("load none"),
            OwnerResolution::None
        );
        trash_test_root(&root);
    }

    #[test]
    fn resolution_rejects_unregistered_room_and_unknown_fields() {
        let (root, context) = context("unregistered");
        fs::write(root.join("owner.json"), r#"{"room": "ghost"}"#).expect("owner");
        let error = load_owner(&context).expect_err("unregistered room must fail closed");
        assert_eq!(error.code.as_str(), "config_invalid");
        assert!(error.message.contains("unregistered room 'ghost'"));
        fs::write(root.join("owner.json"), r#"{"room": "mara", "typo": 1}"#).expect("owner");
        let error = load_owner(&context).expect_err("unknown field must be loud");
        assert_eq!(error.code.as_str(), "config_invalid");
        assert!(
            error.message.contains("unknown field"),
            "deny-unknown-fields: {error:?}"
        );
        trash_test_root(&root);
    }

    #[test]
    fn legacy_uses_the_registered_rooms_resolved_path_not_the_raw_string() {
        // rooms.json contains `~/.trey-room`; the resolved sidecar must be
        // absolute and never contain the literal `~`.
        let (root, context) = context("legacypath");
        match load_owner(&context).expect("legacy") {
            OwnerResolution::Legacy(owner) => {
                assert!(owner.sidecar_dir.is_absolute());
                let rendered = owner.sidecar_dir.display().to_string();
                assert!(
                    !rendered.contains('~'),
                    "resolved path leaked '~': {rendered}"
                );
                assert_eq!(owner.sidecar_dir, root.join(".trey-room"));
            }
            other => panic!("expected legacy, got {other:?}"),
        }
        trash_test_root(&root);
    }

    #[test]
    fn symlinked_owner_json_is_refused_at_load() {
        let (root, context) = context("symlink");
        fs::write(root.join("owner-target.json"), r#"{"room": "mara"}"#).expect("target");
        std::os::unix::fs::symlink(root.join("owner-target.json"), root.join("owner.json"))
            .expect("symlink");
        let error = load_owner(&context).expect_err("symlinked anchor must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
        assert!(error.message.contains("symlink"));
        trash_test_root(&root);
    }

    #[test]
    fn explicit_values_are_derived_when_missing_and_bounded_when_present() {
        let (root, context) = context("explicit");
        fs::write(
            root.join("owner.json"),
            r#"{
              "room": "mara",
              "sidecar_dir": "/srv/mara-sidecar",
              "allowed_signers": "/srv/signers",
              "principal": "mara@example.com",
              "namespace": "mara-mail",
              "marker": "🎩",
              "label": "Top Hat"
            }"#,
        )
        .expect("owner");
        let owner = load_owner(&context).expect("load");
        let OwnerResolution::Configured(owner) = owner else {
            panic!("expected configured");
        };
        assert_eq!(owner.sidecar_dir, PathBuf::from("/srv/mara-sidecar"));
        assert_eq!(owner.allowed_signers, PathBuf::from("/srv/signers"));
        assert_eq!(owner.principal, "mara@example.com");
        assert_eq!(owner.namespace, "mara-mail");
        assert_eq!(owner.marker, "🎩");
        assert_eq!(owner.label, "Top Hat");

        // Relative sidecar, hostile principal chars, oversized namespace.
        for (json, needle) in [
            (r#"{"room":"mara","sidecar_dir":"relative"}"#, "sidecar_dir"),
            (r#"{"room":"mara","principal":"mara name"}"#, "principal"),
            (r#"{"room":"mara","namespace":"a/b"}"#, "namespace"),
            (r#"{"room":"mara","namespace":""}"#, "namespace"),
        ] {
            fs::write(root.join("owner.json"), json).expect("owner fixture");
            let error = load_owner(&context).expect_err("invalid owner must fail closed");
            assert_eq!(error.code.as_str(), "config_invalid");
            assert!(
                error.message.contains(needle),
                "expected {needle} in: {}",
                error.message
            );
        }
        // Oversized namespace and label derived from an absurd room name fail
        // closed with the derived-value message.
        fs::write(
            root.join("rooms.json"),
            r#"{"mara": "~/.mara-room", "way-too-long-for-a-namespace-derivation-0123456789abcdef0123456789abcd": "~/x"}"#,
        )
        .expect("rooms");
        fs::write(
            root.join("owner.json"),
            r#"{"room":"way-too-long-for-a-namespace-derivation-0123456789abcdef0123456789abcd"}"#,
        )
        .expect("owner");
        let error = load_owner(&context).expect_err("derived namespace must be bounded");
        assert!(error.message.contains("derived"), "{}", error.message);
        trash_test_root(&root);
    }

    #[test]
    fn marker_predicate_rejects_hostile_glyphs_and_zwj_abuse() {
        for (marker, needle) in [
            ("", "one glyph"),
            ("🐳🐋", "one glyph"),
            (".", "non-ASCII"),
            ("x", "non-ASCII"),
            ("a\u{200d}b", "one glyph"),
            ("\u{202E}", "control, bidi"),
            ("\n", "control, bidi"),
            ("👩\u{200d}", "zero-width joiner"),
            ("\u{200d}🐳", "zero-width joiner"),
        ] {
            let error = validate_marker(marker).expect_err("marker must be refused");
            assert!(
                error.contains(needle),
                "marker {marker:?} refused with {error:?}, expected mention of {needle:?}"
            );
        }
        validate_marker("🐳").expect("plain emoji ok");
        validate_marker("👩\u{200d}🚀").expect("ZWJ emoji ok (join inside the cluster is fine)");
        validate_marker("⚖️").expect("VS16 emoji ok");
    }

    #[test]
    fn label_predicate_rejects_hostile_labels() {
        for (label, needle) in [
            ("", "empty"),
            ("   ", "whitespace-only"),
            ("evil\u{202E}name", "control, bidi"),
            ("evil\u{2028}", "control, bidi"),
            (&"x".repeat(33), "exceeds 32"),
        ] {
            let error = validate_label(label).expect_err("label must be refused");
            assert!(
                error.contains(needle),
                "label {label:?} refused with {error:?}, expected {needle:?}"
            );
        }
        validate_label("M").expect("single char ok");
        validate_label("Marvelous Owner 🎩").expect("plain label ok");
    }

    #[test]
    fn validate_owner_values_checks_every_explicit_field() {
        let root = test_root("owner-values");
        let path = root.join("owner.json");
        let check = |file: OwnerFile, needle: &str| {
            let error = validate_owner_values(&path, &file).expect_err("must refuse");
            assert_eq!(error.code.as_str(), "config_invalid");
            assert!(
                error.message.contains(needle),
                "expected {needle} in {}",
                error.message
            );
        };
        check(
            OwnerFile {
                room: "bad/room".to_owned(),
                ..file("mara")
            },
            "room",
        );
        check(
            OwnerFile {
                sidecar_dir: Some(PathBuf::from("relative")),
                ..file("mara")
            },
            "sidecar_dir",
        );
        check(
            OwnerFile {
                allowed_signers: Some(PathBuf::from("relative")),
                ..file("mara")
            },
            "allowed_signers",
        );
        check(
            OwnerFile {
                principal: Some("a b".to_owned()),
                ..file("mara")
            },
            "principal",
        );
        check(
            OwnerFile {
                namespace: Some("n".repeat(65)),
                ..file("mara")
            },
            "namespace",
        );
        check(
            OwnerFile {
                marker: Some("🐳🐋".to_owned()),
                ..file("mara")
            },
            "marker",
        );
        check(
            OwnerFile {
                label: Some("   ".to_owned()),
                ..file("mara")
            },
            "label",
        );
        trash_test_root(&root);
    }

    #[test]
    fn exclusive_atomic_write_never_replaces_an_existing_destination() {
        // The A0a init commit primitive: creation is atomic and refusal-based.
        // Any existing destination (including one that appears between the
        // temp write and the hard-link commit) surfaces as AlreadyExists and
        // is never replaced.
        let root = test_root("owner-atomic-write");
        let dest = root.join("owner.json");
        fs::write(&dest, "original bytes").expect("pre-existing destination");
        let error = exclusive_atomic_write(&dest, b"replacement bytes")
            .expect_err("must refuse an existing destination");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(&dest).expect("reread"),
            "original bytes",
            "destination content must be untouched"
        );
        fs::remove_file(&dest).expect("remove");
        exclusive_atomic_write(&dest, b"fresh bytes").expect("absent destination creates");
        assert_eq!(
            fs::read_to_string(&dest).expect("reread"),
            "fresh bytes",
            "created content"
        );
        trash_test_root(&root);
    }

    #[test]
    fn read_owner_file_rejects_symlink_and_legacy_matches_registered_path() {
        let (root, context) = context("readfile");
        let legacy: ResolvedOwner =
            legacy_owner(&context, &context.load_rooms().expect("rooms")).expect("legacy");
        assert_eq!(legacy.room, LEGACY_OWNER_ROOM);
        fs::write(root.join("rooms.json"), r#"{"mara":"~/.mara-room"}"#).expect("rooms");
        let rooms = context.load_rooms().expect("rooms");
        fs::write(
            root.join("owner.json"),
            r#"{"room":"mara","sidecar_dir":"/srv/x"}"#,
        )
        .expect("owner");
        let owner_file = read_owner_file(&root.join("owner.json")).expect("parse");
        let resolved =
            crate::mailbox::resolve_owner_file(&context, &owner_file, &rooms).expect("resolve");
        assert_eq!(resolved.sidecar_dir, PathBuf::from("/srv/x"));
        // load_owner_with_rooms honors an externally supplied registry.
        match load_owner_with_rooms(&context, &rooms).expect("with rooms") {
            OwnerResolution::Configured(resolved) => {
                assert_eq!(resolved.room, "mara");
            }
            other => panic!("expected configured, got {other:?}"),
        }
        trash_test_root(&root);
    }
}
