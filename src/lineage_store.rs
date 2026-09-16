use crate::error::{AppError, AppResult, ErrorCode};
use crate::lineage::{self, Lineage, Member, LINEAGES_DIR};
use crate::mailbox::{atomic_replace, local_timestamp, Context};
use crate::participant::{self, Participant};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const LINEAGE_VERSION: u64 = 1;
const LINEAGE_FILE: &str = "lineage.json";
const HISTORY_FILE: &str = "history.jsonl";
const VOICES_DIR: &str = "voices";
const TERMS_FILE: &str = "terms.md";
const VOICE_MAX_BYTES: u64 = 4096;

#[derive(Debug, Serialize)]
struct LineageRecord<'a> {
    version: u64,
    name: &'a str,
    created: &'a str,
    founder: &'a str,
    host: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JournalEntry {
    pub at: String,
    pub event: String,
    pub participant: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct VoiceIndex {
    pub participant: String,
    pub revisions: usize,
    pub withdrawn_gaps: usize,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LineageSummary {
    pub name: String,
    pub affiliates: usize,
    pub voices: usize,
    pub terms: bool,
    pub founder: String,
}

#[derive(Debug)]
pub(crate) struct LineageView {
    pub lineage: Lineage,
    pub members: BTreeMap<String, Member>,
    pub voices: Vec<VoiceIndex>,
    pub terms: bool,
}

#[derive(Debug)]
pub(crate) enum ContinueResult {
    Affiliated {
        lineage: Lineage,
        participant: String,
        changed: bool,
    },
    NeedsAcknowledgement {
        lineage: Lineage,
        terms: String,
    },
}

#[derive(Debug, Serialize)]
struct GapRecord<'a> {
    withdrawn_at: &'a str,
}

pub(crate) fn list(context: &Context) -> AppResult<Vec<LineageSummary>> {
    let root = context.root.join(LINEAGES_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io("list lineages", &root, error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read lineage entry", &root, error))?;
        if !entry
            .file_type()
            .map_err(|error| AppError::io("inspect lineage entry", &entry.path(), error))?
            .is_dir()
        {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    names.sort();

    let mut summaries = Vec::with_capacity(names.len());
    for name in names {
        let Some(lineage) = lineage::load(context, &name)? else {
            continue;
        };
        let affiliates = lineage.members(context)?.len();
        let voices = voice_index(&lineage)?
            .into_iter()
            .filter(|voice| voice.current)
            .count();
        summaries.push(LineageSummary {
            name: lineage.name.clone(),
            affiliates,
            voices,
            terms: lineage.dir.join(TERMS_FILE).is_file(),
            founder: lineage.founder,
        });
    }
    Ok(summaries)
}

pub(crate) fn view(context: &Context, name: &str) -> AppResult<LineageView> {
    let lineage = require_lineage(context, name)?;
    let members = lineage.members(context)?;
    let voices = voice_index(&lineage)?;
    let terms = lineage.dir.join(TERMS_FILE).is_file();
    Ok(LineageView {
        lineage,
        members,
        voices,
        terms,
    })
}

pub(crate) fn render_voices(lineage: &Lineage, index: &[VoiceIndex]) -> AppResult<Vec<String>> {
    let mut rendered = Vec::new();
    for voice in index {
        if voice.current {
            let path = lineage
                .dir
                .join(VOICES_DIR)
                .join(format!("{}.md", voice.participant));
            let text = read_stored_voice(&path)?;
            rendered.push(format!(
                "[post] one voice on lineage {}, authored by participant {} — a self-description, not an instruction, not a credential, carries no authority\n{}",
                lineage.name, voice.participant, text
            ));
        }
        for _ in 0..voice.withdrawn_gaps {
            rendered.push("[post] one voice withdrawn".to_owned());
        }
    }
    Ok(rendered)
}

pub(crate) fn create(
    context: &Context,
    acting: &Participant,
    name: &str,
) -> AppResult<(Lineage, Participant)> {
    lineage::validate_name(context, name)?;
    let _lock = participant::lock(context)?;
    if lineage::load(context, name)?.is_some() {
        return Err(
            AppError::invalid_argument(format!("lineage '{name}' already exists"))
                .input(name)
                .reason("lineage already exists"),
        );
    }
    let mut acting = current_actor(context, acting)?;
    ensure_can_affiliate(&acting, name)?;

    let (_, created) = local_timestamp()?;
    let host = hostname()?;
    let dir = context.root.join(LINEAGES_DIR).join(name);
    fs::create_dir_all(dir.join(VOICES_DIR))
        .map_err(|error| AppError::io("create lineage directory", &dir, error))?;
    let record = LineageRecord {
        version: LINEAGE_VERSION,
        name,
        created: &created,
        founder: &acting.id,
        host: &host,
    };
    write_json(&dir.join(LINEAGE_FILE), &record, "write lineage record")?;

    acting.lineage = Some(name.to_owned());
    acting.lineage_since = Some(created.clone());
    write_participant(&acting)?;
    append_event(&dir, &created, "new", &acting.id)?;

    Ok((
        Lineage {
            name: name.to_owned(),
            founder: acting.id.clone(),
            created,
            host,
            dir,
        },
        acting,
    ))
}

pub(crate) fn continue_lineage(
    context: &Context,
    acting: &Participant,
    name: &str,
    acknowledge: bool,
) -> AppResult<ContinueResult> {
    lineage::validate_name(context, name)?;
    let _lock = participant::lock(context)?;
    let lineage = require_lineage(context, name)?;
    let mut acting = current_actor(context, acting)?;
    ensure_can_affiliate(&acting, name)?;

    let terms_path = lineage.dir.join(TERMS_FILE);
    if terms_path.is_file() && !acknowledge {
        return Ok(ContinueResult::NeedsAcknowledgement {
            lineage,
            terms: read_text_file(&terms_path, "read lineage terms")?,
        });
    }
    if acting.lineage.as_deref() == Some(name) {
        return Ok(ContinueResult::Affiliated {
            lineage,
            participant: acting.id,
            changed: false,
        });
    }

    let (_, at) = local_timestamp()?;
    acting.lineage = Some(name.to_owned());
    acting.lineage_since = Some(at.clone());
    write_participant(&acting)?;
    append_event(&lineage.dir, &at, "continue", &acting.id)?;
    Ok(ContinueResult::Affiliated {
        lineage,
        participant: acting.id,
        changed: true,
    })
}

pub(crate) fn leave(
    context: &Context,
    acting: &Participant,
) -> AppResult<(Participant, Option<String>, bool)> {
    let _lock = participant::lock(context)?;
    let mut acting = current_actor(context, acting)?;
    let Some(name) = acting.lineage.clone() else {
        return Ok((acting, None, false));
    };
    let lineage = require_lineage(context, &name)?;
    let (_, at) = local_timestamp()?;
    acting.lineage = None;
    acting.lineage_since = None;
    write_participant(&acting)?;
    append_event(&lineage.dir, &at, "leave", &acting.id)?;
    Ok((acting, Some(name), true))
}

pub(crate) fn add_voice(
    context: &Context,
    acting: &Participant,
    author: &str,
    body_file: &Path,
) -> AppResult<(String, usize)> {
    ensure_own_voice(acting, author)?;
    let body = read_voice_input(body_file)?;
    let _lock = participant::lock(context)?;
    let acting = current_actor(context, acting)?;
    ensure_own_voice(&acting, author)?;
    let lineage = acting_lineage(context, &acting)?;
    let voices = lineage.dir.join(VOICES_DIR);
    fs::create_dir_all(&voices)
        .map_err(|error| AppError::io("create lineage voices directory", &voices, error))?;

    let current = voices.join(format!("{author}.md"));
    let history = voices.join(format!("{author}.history"));
    let mut revisions = history_revision_count(&history)?;
    if current.is_file() {
        let prior = fs::read(&current)
            .map_err(|error| AppError::io("read prior lineage voice", &current, error))?;
        fs::create_dir_all(&history)
            .map_err(|error| AppError::io("create voice history directory", &history, error))?;
        let revision = next_history_revision(&history)?;
        let revision_path = history.join(format!("{revision}.md"));
        atomic_replace(&revision_path, &prior)
            .map_err(|error| AppError::io("write voice history revision", &revision_path, error))?;
        revisions += 1;
    }
    atomic_replace(&current, body.as_bytes())
        .map_err(|error| AppError::io("write lineage voice", &current, error))?;
    remove_file_if_exists(
        &voices.join(format!("{author}.gap")),
        "clear voice withdrawal gap",
    )?;
    let (_, at) = local_timestamp()?;
    append_event(
        &lineage.dir,
        &at,
        if revisions == 0 {
            "voice_add"
        } else {
            "voice_revise"
        },
        &acting.id,
    )?;
    Ok((lineage.name, revisions))
}

pub(crate) fn withdraw_voice(
    context: &Context,
    acting: &Participant,
    author: &str,
) -> AppResult<String> {
    ensure_own_voice(acting, author)?;
    let _lock = participant::lock(context)?;
    let acting = current_actor(context, acting)?;
    ensure_own_voice(&acting, author)?;
    let lineage = acting_lineage(context, &acting)?;
    let voices = lineage.dir.join(VOICES_DIR);
    let current = voices.join(format!("{author}.md"));
    let history = voices.join(format!("{author}.history"));
    let gap = voices.join(format!("{author}.gap"));
    if !current.exists() && !history.exists() && !gap.exists() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("participant '{author}' has no voice to withdraw"),
            "Add your own voice first with `post identity voice add --body-file PATH`.",
        )
        .reason("the acting participant has no current voice or withdrawal gap"));
    }

    remove_file_if_exists(&current, "remove current lineage voice")?;
    remove_dir_if_exists(&history, "remove lineage voice history")?;
    fs::create_dir_all(&voices)
        .map_err(|error| AppError::io("create lineage voices directory", &voices, error))?;
    let (_, at) = local_timestamp()?;
    write_json(
        &gap,
        &GapRecord { withdrawn_at: &at },
        "write voice withdrawal gap",
    )?;
    append_event(&lineage.dir, &at, "voice_withdraw", &acting.id)?;
    Ok(lineage.name)
}

pub(crate) fn set_terms(
    context: &Context,
    acting: &Participant,
    body_file: &Path,
) -> AppResult<String> {
    let body = read_text_file(body_file, "read lineage terms input")?;
    let _lock = participant::lock(context)?;
    let acting = current_actor(context, acting)?;
    let lineage = acting_lineage(context, &acting)?;
    let path = lineage.dir.join(TERMS_FILE);
    atomic_replace(&path, body.as_bytes())
        .map_err(|error| AppError::io("write lineage terms", &path, error))?;
    let (_, at) = local_timestamp()?;
    append_event(&lineage.dir, &at, "terms_set", &acting.id)?;
    Ok(lineage.name)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read_history(lineage: &Lineage) -> AppResult<Vec<JournalEntry>> {
    let path = lineage.dir.join(HISTORY_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io("read lineage journal", &path, error)),
    };
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let last = lines.len().saturating_sub(1);
    let mut entries = Vec::with_capacity(lines.len());
    for (index, line) in lines.into_iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(entry) => entries.push(entry),
            Err(_) if index == last => break,
            Err(error) => {
                return Err(AppError::config(
                    &path,
                    format!(
                        "invalid lineage journal entry on line {}: {error}",
                        index + 1
                    ),
                ))
            }
        }
    }
    Ok(entries)
}

fn current_actor(context: &Context, acting: &Participant) -> AppResult<Participant> {
    participant::load(context, &acting.id)?.ok_or_else(|| {
        AppError::no_participant("run: post participant bind")
            .reason("the acting participant record disappeared before the mutation")
    })
}

fn ensure_can_affiliate(acting: &Participant, requested: &str) -> AppResult<()> {
    if let Some(current) = &acting.lineage {
        if current != requested {
            return Err(AppError::invalid_argument(format!(
                "participant '{}' is already affiliated with lineage '{}'; run: post identity leave",
                acting.id, current
            ))
            .exact_fix("post identity leave")
            .reason("a participant may have only one current lineage affiliation"));
        }
    }
    Ok(())
}

fn ensure_own_voice(acting: &Participant, author: &str) -> AppResult<()> {
    if acting.id != author {
        return Err(AppError::invalid_argument(format!(
            "participant '{}' may edit only its own voice, not participant '{author}'",
            acting.id
        ))
        .reason("a participant edits only its own voice"));
    }
    Ok(())
}

fn acting_lineage(context: &Context, acting: &Participant) -> AppResult<Lineage> {
    let name = acting.lineage.as_deref().ok_or_else(|| {
        AppError::invalid_argument(format!(
            "participant '{}' is not affiliated with a lineage",
            acting.id
        ))
        .reason("voice and terms changes require a current lineage affiliation")
    })?;
    require_lineage(context, name)
}

fn require_lineage(context: &Context, name: &str) -> AppResult<Lineage> {
    lineage::load(context, name)?.ok_or_else(|| {
        AppError::new(
            ErrorCode::NotFound,
            format!("lineage '{name}' was not found"),
            "Run `post identity list` and retry with an existing lineage name.",
        )
        .input(name)
        .reason("lineage record is absent")
    })
}

fn voice_index(lineage: &Lineage) -> AppResult<Vec<VoiceIndex>> {
    let root = lineage.dir.join(VOICES_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io("list lineage voices", &root, error)),
    };
    let mut authors = BTreeSet::new();
    let mut current = BTreeSet::new();
    let mut gaps = BTreeSet::new();
    let mut histories = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read voice entry", &root, error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| AppError::io("inspect voice entry", &entry.path(), error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if file_type.is_file() {
            if let Some(author) = name.strip_suffix(".md") {
                authors.insert(author.to_owned());
                current.insert(author.to_owned());
            } else if let Some(author) = name.strip_suffix(".gap") {
                authors.insert(author.to_owned());
                gaps.insert(author.to_owned());
            }
        } else if file_type.is_dir() {
            if let Some(author) = name.strip_suffix(".history") {
                authors.insert(author.to_owned());
                histories.insert(author.to_owned(), history_revision_count(&entry.path())?);
            }
        }
    }
    Ok(authors
        .into_iter()
        .map(|participant| VoiceIndex {
            revisions: histories.get(&participant).copied().unwrap_or(0),
            withdrawn_gaps: usize::from(gaps.contains(&participant)),
            current: current.contains(&participant),
            participant,
        })
        .collect())
}

fn history_revision_count(path: &Path) -> AppResult<usize> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(AppError::io("list voice history", path, error)),
    };
    let mut count = 0;
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read voice history entry", path, error))?;
        if entry
            .file_type()
            .map_err(|error| AppError::io("inspect voice history entry", &entry.path(), error))?
            .is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("md")
        {
            count += 1;
        }
    }
    Ok(count)
}

fn next_history_revision(path: &Path) -> AppResult<u64> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(1),
        Err(error) => return Err(AppError::io("list voice history", path, error)),
    };
    let mut highest = 0;
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read voice history entry", path, error))?;
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if path.extension().and_then(|value| value.to_str()) == Some("md") {
            if let Ok(revision) = stem.parse::<u64>() {
                highest = highest.max(revision);
            }
        }
    }
    highest.checked_add(1).ok_or_else(|| {
        AppError::new(
            ErrorCode::IoError,
            "voice history revision number overflowed",
            "Preserve the voice history and report this lineage for repair.",
        )
    })
}

fn read_voice_input(path: &Path) -> AppResult<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| AppError::io("open voice body file", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| AppError::io("inspect voice body file", path, error))?;
    if !metadata.is_file() {
        return Err(AppError::invalid_argument(format!(
            "voice body '{}' must be a regular file",
            path.display()
        ))
        .path(path.display().to_string())
        .reason("voice body files must be regular files; symlinks are rejected"));
    }
    if metadata.len() > VOICE_MAX_BYTES {
        return Err(AppError::invalid_argument(format!(
            "voice body '{}' is {} bytes; the maximum is {VOICE_MAX_BYTES}",
            path.display(),
            metadata.len()
        ))
        .path(path.display().to_string())
        .reason("voice body exceeds the 4096-byte cap"));
    }
    read_voice_from_file(file, metadata.len(), path, "voice body")
}

fn read_stored_voice(path: &Path) -> AppResult<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| AppError::io("open stored lineage voice", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| AppError::io("inspect stored lineage voice", path, error))?;
    if !metadata.is_file() || metadata.len() > VOICE_MAX_BYTES {
        return Err(AppError::config(
            path,
            "stored lineage voice is not a regular file within the 4096-byte cap",
        ));
    }
    read_voice_from_file(file, metadata.len(), path, "stored lineage voice")
}

fn read_voice_from_file(
    mut file: File,
    length: u64,
    path: &Path,
    label: &str,
) -> AppResult<String> {
    let mut bytes = Vec::with_capacity(length as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| AppError::io(&format!("read {label}"), path, error))?;
    if bytes.len() as u64 != length {
        return Err(AppError::io(
            &format!("read {label}"),
            path,
            "file length changed while it was held open",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        AppError::invalid_argument(format!("{label} '{}' is not valid UTF-8", path.display()))
            .path(path.display().to_string())
            .reason(error.to_string())
    })?;
    if text
        .chars()
        .any(|character| character.is_ascii_control() && !matches!(character, '\t' | '\n' | '\r'))
    {
        return Err(AppError::invalid_argument(format!(
            "{label} '{}' contains control characters",
            path.display()
        ))
        .path(path.display().to_string())
        .reason("only tab, LF, and CR controls are allowed"));
    }
    Ok(text)
}

fn read_text_file(path: &Path, operation: &str) -> AppResult<String> {
    let bytes = fs::read(path).map_err(|error| AppError::io(operation, path, error))?;
    String::from_utf8(bytes).map_err(|error| {
        AppError::invalid_argument(format!("'{}' is not valid UTF-8", path.display()))
            .path(path.display().to_string())
            .reason(error.to_string())
    })
}

fn write_participant(participant: &Participant) -> AppResult<()> {
    let path = participant.dir.join("participant.json");
    write_json(&path, participant, "write participant affiliation")
}

fn write_json(path: &Path, value: &impl Serialize, operation: &str) -> AppResult<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AppError::io(&format!("serialize {operation}"), path, error))?;
    bytes.push(b'\n');
    atomic_replace(path, &bytes).map_err(|error| AppError::io(operation, path, error))
}

fn append_event(dir: &Path, at: &str, event: &str, participant: &str) -> AppResult<()> {
    let path = dir.join(HISTORY_FILE);
    let mut bytes = serde_json::to_vec(&JournalEntry {
        at: at.to_owned(),
        event: event.to_owned(),
        participant: participant.to_owned(),
    })
    .map_err(|error| AppError::io("serialize lineage journal entry", &path, error))?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| AppError::io("open lineage journal", &path, error))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| AppError::io("append and sync lineage journal", &path, error))
}

fn remove_file_if_exists(path: &Path, operation: &str) -> AppResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io(operation, path, error)),
    }
}

fn remove_dir_if_exists(path: &Path, operation: &str) -> AppResult<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io(operation, path, error)),
    }
}

fn hostname() -> AppResult<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is valid for its declared length, and gethostname
    // writes at most that many bytes without retaining the pointer.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return Err(AppError::io(
            "read host name",
            Path::new("<hostname>"),
            std::io::Error::last_os_error(),
        ));
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8(buffer[..length].to_vec()).map_err(|error| {
        AppError::new(
            ErrorCode::ConfigInvalid,
            format!("host name is not valid UTF-8: {error}"),
            "Set the machine host name to valid UTF-8 and retry.",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};

    fn participant(root: &Path, id: &str, lineage: Option<&str>) -> Participant {
        let dir = root.join("participants").join(id);
        fs::create_dir_all(&dir).expect("participant directory");
        Participant {
            version: 1,
            id: id.to_owned(),
            harness: "test".to_owned(),
            conversation_key_digest: "0".repeat(64),
            created: "2026-09-16 00:00:00 +0000".to_owned(),
            workspace: None,
            workspace_path: None,
            lineage: lineage.map(str::to_owned),
            lineage_since: lineage.map(|_| "2026-09-16 00:00:00 +0000".to_owned()),
            display_name: None,
            dir,
        }
    }

    #[test]
    fn lineage_foreign_voice_edit_is_refused() {
        let root = test_root("lineage-foreign-voice");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        fs::create_dir_all(&context.root).expect("mail root");
        fs::write(context.root.join("rooms.json"), "{}\n").expect("rooms");
        let acting = participant(&root, "actor", Some("ember"));
        write_participant(&acting).expect("participant record");
        let lineage_dir = root.join("lineages/ember");
        fs::create_dir_all(lineage_dir.join(VOICES_DIR)).expect("lineage voices directory");
        fs::write(
            lineage_dir.join(LINEAGE_FILE),
            concat!(
                "{\"version\":1,\"name\":\"ember\",",
                "\"created\":\"now\",\"founder\":\"actor\",\"host\":\"test\"}\n"
            ),
        )
        .expect("lineage record");
        let body = root.join("voice.md");
        fs::write(&body, "foreign edit").expect("voice body");

        let error = add_voice(&context, &acting, "other", &body)
            .expect_err("foreign voice author must be refused");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("only its own voice"));
        assert!(!root.join("lineages/ember/voices/other.md").exists());
        trash_test_root(&root);
    }

    #[test]
    fn lineage_journal_ignores_only_a_malformed_final_line() {
        let root = test_root("lineage-journal");
        let dir = root.join("lineages/ember");
        fs::create_dir_all(&dir).expect("lineage directory");
        let lineage = Lineage {
            name: "ember".to_owned(),
            founder: "actor".to_owned(),
            created: "2026-09-16 00:00:00 +0000".to_owned(),
            host: "test".to_owned(),
            dir: dir.clone(),
        };
        fs::write(
            dir.join(HISTORY_FILE),
            concat!(
                "{\"at\":\"now\",\"event\":\"new\",\"participant\":\"actor\"}\n",
                "{\"at\":\"later\""
            ),
        )
        .expect("journal");
        let history = read_history(&lineage).expect("truncated tail is tolerated");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].event, "new");

        fs::write(
            dir.join(HISTORY_FILE),
            concat!(
                "not-json\n",
                "{\"at\":\"now\",\"event\":\"new\",\"participant\":\"actor\"}\n"
            ),
        )
        .expect("corrupt journal");
        assert!(read_history(&lineage).is_err());
        trash_test_root(&root);
    }
}
