pub mod cli;
pub mod commands;
pub mod error;
pub mod output;

use clap::error::ErrorKind;
use clap::Parser;
use error::{AppError, AppResult, ErrorCode};
use output::{BlockingRuleOutput, Envelope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_RULES_JSON: &str = r#"{
  "blocked": [
    {
      "from": "*",
      "to": "agent-memory",
      "reason": "ARMED INSTRUMENT: no contact with the Memorum room until its arc closeout exists (claude-space JOURNAL 2026-07-12 tick 24). Remove this rule only after the closeout is written and the affect check has fired."
    }
  ]
}
"#;

const DEFAULT_ROOMS_JSON: &str = r#"{
  "claude-space": "~/Code/claude-space",
  "pact": "~/Library/CloudStorage/Dropbox/Prospera/Policy/pact-act",
  "agent-memory": "~/Code/agent-memory"
}
"#;

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Context {
    pub root: PathBuf,
    pub home: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingRule {
    pub from: String,
    pub to: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesConfig {
    pub blocked: Vec<BlockingRule>,
}

#[derive(Debug)]
pub struct ParsedMail {
    pub envelope: Envelope,
    pub body: String,
}

impl From<&BlockingRule> for BlockingRuleOutput {
    fn from(rule: &BlockingRule) -> Self {
        Self {
            from: rule.from.clone(),
            to: rule.to.clone(),
            reason: rule.reason.clone(),
        }
    }
}

impl Context {
    pub fn from_env() -> AppResult<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            AppError::new(
                ErrorCode::ConfigInvalid,
                "environment variable 'HOME' is missing, so '~' room paths cannot be resolved",
                "Set `HOME` to an absolute directory and retry the same command.",
            )
            .detail("input", "HOME")
            .detail("reason", "missing environment variable")
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
            .detail("input", root.display().to_string())
            .detail("reason", "path is relative"));
        }
        Ok(Self { root, home })
    }

    pub fn prepare_first_run(&self) -> AppResult<()> {
        if self.root.exists() {
            return Ok(());
        }
        fs::create_dir_all(&self.root)
            .map_err(|error| AppError::io("create mailbox root", &self.root, error))?;
        self.write_default_if_missing("rules.json", DEFAULT_RULES_JSON)?;
        self.write_default_if_missing("rooms.json", DEFAULT_ROOMS_JSON)?;
        Ok(())
    }

    pub fn write_default_if_missing(&self, name: &str, contents: &str) -> AppResult<bool> {
        let path = self.root.join(name);
        if path.exists() {
            return Ok(false);
        }
        atomic_write(&path, contents.as_bytes())?;
        Ok(true)
    }

    pub fn load_rooms(&self) -> AppResult<BTreeMap<String, String>> {
        let path = self.root.join("rooms.json");
        let bytes = fs::read(&path)
            .map_err(|error| AppError::config(&path, format!("cannot read file: {error}")))?;
        let rooms: BTreeMap<String, String> = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::config(&path, format!("invalid JSON object: {error}")))?;
        if rooms.is_empty() {
            return Err(AppError::config(&path, "room map is empty"));
        }
        for (name, room_path) in &rooms {
            validate_component(name).map_err(|reason| AppError::config(&path, reason))?;
            self.expand_room_path(room_path)
                .map_err(|reason| AppError::config(&path, reason))?;
        }
        Ok(rooms)
    }

    pub fn load_rules(&self, rooms: &BTreeMap<String, String>) -> AppResult<RulesConfig> {
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

    pub fn expand_room_path(&self, value: &str) -> Result<PathBuf, String> {
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

    pub fn resolved_room(
        &self,
        explicit: Option<String>,
        rooms: &BTreeMap<String, String>,
    ) -> AppResult<String> {
        if let Some(room) = explicit {
            validate_component(&room).map_err(|reason| {
                AppError::new(
                    ErrorCode::InvalidArgument,
                    format!("room '{room}' is invalid: {reason}"),
                    "Pass a single path-safe room name without '/' or '\\'.",
                )
                .detail("input", room.clone())
                .detail("reason", reason)
            })?;
            return Ok(room);
        }
        self.infer_from_cwd(rooms)
    }

    pub fn infer_from_cwd(&self, rooms: &BTreeMap<String, String>) -> AppResult<String> {
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
                .detail("input", cwd.display().to_string())
                .detail("reason", "missing UTF-8 basename")
            })?;
        validate_component(basename).map_err(|reason| {
            AppError::new(
                ErrorCode::InvalidArgument,
                format!("inferred name '{basename}' is invalid: {reason}"),
                "Pass `--from <NAME>` for send or `--room <ROOM>` for inbox/read.",
            )
            .detail("input", basename)
            .detail("reason", reason)
        })?;
        Ok(basename.to_owned())
    }

    pub fn ensure_sender_allowed(
        &self,
        sender: &str,
        rooms: &BTreeMap<String, String>,
    ) -> AppResult<()> {
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
        .detail("input", sender)
        .detail("reason", "registered room name used outside its room tree")
        .detail("registered_path", expanded.display().to_string()))
    }

    pub fn mailbox_dirs(&self, room: &str) -> AppResult<(PathBuf, PathBuf)> {
        validate_component(room).map_err(|reason| {
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

pub fn entry<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match cli::Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                return if error.print().is_ok() { 0 } else { 70 };
            }
            _ => {
                let error = AppError::invalid_argument(error.to_string().trim().to_owned())
                    .detail("reason", "command-line parse failure");
                output::write_error(&error, false);
                return error.exit_code;
            }
        },
    };
    let pretty = cli.pretty;
    match commands::execute(cli) {
        Ok(result) => match output::write_stdout(&result.stdout) {
            Ok(()) => result.exit_code,
            Err(source) => {
                let error = AppError::io("write stdout", Path::new("<stdout>"), source);
                output::write_error(&error, pretty);
                error.exit_code
            }
        },
        Err(error) => {
            output::write_error(&error, pretty);
            error.exit_code
        }
    }
}

pub fn validate_component(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("name is empty".to_owned());
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

fn path_contains(root: &Path, candidate: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let candidate = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    candidate.starts_with(root)
}

pub fn parse_mail(path: &Path) -> AppResult<ParsedMail> {
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

fn validate_envelope(path: &Path, envelope: &Envelope) -> AppResult<()> {
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
    Ok(())
}

pub fn mail_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
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

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let parent = path.parent().ok_or_else(|| {
        AppError::new(
            ErrorCode::IoError,
            format!(
                "cannot atomically write '{}': path has no parent",
                path.display()
            ),
            "Pass a path inside an existing directory and retry.",
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::IoError,
                format!(
                    "cannot atomically write '{}': filename is not UTF-8",
                    path.display()
                ),
                "Use a UTF-8 filename and retry.",
            )
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
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(AppError::io("atomically write file", path, source));
    }
    Ok(())
}

pub fn encode_mail(envelope: &Envelope, body: &str) -> AppResult<Vec<u8>> {
    let mut payload = serde_json::to_string_pretty(envelope).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!(
                "failed to serialize mail envelope '{}': {error}",
                envelope.id
            ),
            "Retry the same send command; if this repeats, report `post --version`.",
        )
    })?;
    payload.push_str("\n---\n");
    payload.push_str(body);
    Ok(payload.into_bytes())
}

pub fn local_timestamp() -> AppResult<(String, String)> {
    let output = ProcessCommand::new("date")
        .arg("+%Y%m%d-%H%M%S|%Y-%m-%d %H:%M:%S %z")
        .output()
        .map_err(|error| AppError::io("run local `date` command", Path::new("date"), error))?;
    if !output.status.success() {
        return Err(AppError::new(
            ErrorCode::IoError,
            format!("local `date` command failed with status {}", output.status),
            "Ensure the system `date` command is on PATH, then retry the same send command.",
        )
        .detail("input", "date")
        .detail("reason", output.status.to_string()));
    }
    let rendered = String::from_utf8(output.stdout).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!("local `date` command returned non-UTF-8 output: {error}"),
            "Use a system `date` implementation that emits UTF-8 and retry.",
        )
    })?;
    let (id_time, sent) = rendered.trim().split_once('|').ok_or_else(|| {
        AppError::new(
            ErrorCode::IoError,
            format!(
                "local `date` output '{}' did not contain the expected separator",
                rendered.trim()
            ),
            "Ensure the system `date` command supports '+FORMAT', then retry.",
        )
        .detail("input", rendered.trim())
        .detail("reason", "missing '|' separator")
    })?;
    Ok((id_time.to_owned(), sent.to_owned()))
}

pub fn new_mail_id(timestamp: &str, attempt: u64) -> AppResult<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            AppError::new(
                ErrorCode::IoError,
                format!("system clock is before the Unix epoch: {error}"),
                "Correct the system clock and retry the send command.",
            )
        })?
        .as_nanos() as u64;
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = nanos ^ counter.rotate_left(17) ^ u64::from(std::process::id()) ^ attempt;
    Ok(format!("{timestamp}-{:06x}", mixed & 0x00ff_ffff))
}

pub fn closest_room<'a>(input: &str, rooms: &'a BTreeMap<String, String>) -> Option<&'a str> {
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

#[cfg(test)]
mod tests {
    use super::atomic_write;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn atomic_write_failure_leaves_no_temporary_or_partial_file() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should follow Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("post-atomic-{nonce}"));
        fs::create_dir_all(&root).expect("create atomic-write test root");
        let destination = root.join("message.mail");
        fs::create_dir(&destination).expect("create rename-blocking destination directory");

        let error = atomic_write(&destination, b"partial data")
            .expect_err("renaming a file over a directory must fail");

        assert_eq!(error.code.as_str(), "io_error");
        let entries: Vec<_> = fs::read_dir(&root)
            .expect("list atomic-write test root")
            .map(|entry| entry.expect("read test entry").file_name())
            .collect();
        assert_eq!(entries, vec!["message.mail"]);
        let cleanup = std::process::Command::new("trash")
            .arg(&root)
            .status()
            .expect("run recoverable test cleanup");
        assert!(
            cleanup.success(),
            "trash should clean atomic-write test root"
        );
    }
}
