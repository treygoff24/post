use crate::error::{AppError, AppResult, ErrorCode};
use crate::lineage::{self, Lineage, Member, LINEAGES_DIR};
use crate::mailbox::{atomic_replace, local_timestamp, shell_quote, validate_room_name, Context};
use crate::participant::{self, Participant};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct VoiceIndex {
    pub participant: String,
    pub revisions: usize,
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
    pub withdrawn_voices: usize,
    pub terms: TermsView,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TermsView {
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TermsDocument {
    pub text: String,
    pub digest: String,
}

#[derive(Debug)]
pub(crate) struct LineageList {
    pub lineages: Vec<LineageSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct Mutation<T> {
    pub value: T,
    pub changed: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct VoiceChange {
    pub lineage: String,
    pub revisions: Option<usize>,
    pub hint: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ContinueResult {
    Affiliated {
        lineage: Lineage,
        participant: String,
        changed: bool,
        warnings: Vec<String>,
        terms: Option<TermsDocument>,
    },
    NeedsAcknowledgement {
        lineage: Lineage,
        terms: TermsDocument,
    },
}

#[derive(Debug)]
pub(crate) enum CreateResult {
    Affiliated(Box<Mutation<(Lineage, Participant)>>),
    NeedsAcknowledgement {
        lineage: Lineage,
        terms: TermsDocument,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct GapRecord {
    version: u64,
    withdrawals: usize,
    cleanup_pending: bool,
}

#[derive(Debug)]
struct VoiceCatalog {
    voices: Vec<VoiceIndex>,
    withdrawn_voices: usize,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct GapCleanup {
    completed_pending: bool,
}

#[derive(Clone, Copy)]
enum InvalidGapPolicy {
    Fail,
    WarnAndHide,
}

pub(crate) fn list(context: &Context) -> AppResult<LineageList> {
    let root = context.root.join(LINEAGES_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LineageList {
                lineages: Vec::new(),
                warnings: Vec::new(),
            })
        }
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
    let mut warnings = Vec::new();
    for name in names {
        let lineage = match lineage::load(context, &name) {
            Ok(Some(lineage)) => lineage,
            Ok(None) => continue,
            Err(error) => {
                warnings.push(format!("lineage '{name}' was skipped: {}", error.message));
                continue;
            }
        };
        let affiliates = match lineage.members(context) {
            Ok(members) => members.len(),
            Err(error) => {
                warnings.push(format!(
                    "lineage '{name}' was skipped: members could not be read: {}",
                    error.message
                ));
                continue;
            }
        };
        let voices = match voice_catalog(&lineage, InvalidGapPolicy::Fail) {
            Ok(catalog) => catalog.voices.len(),
            Err(error) => {
                warnings.push(format!(
                    "lineage '{name}' was skipped: voices could not be read: {}",
                    error
                        .details
                        .reason
                        .as_deref()
                        .unwrap_or("invalid voice index")
                ));
                continue;
            }
        };
        summaries.push(LineageSummary {
            name: lineage.name.clone(),
            affiliates,
            voices,
            terms: lineage.dir.join(TERMS_FILE).is_file(),
            founder: lineage.founder,
        });
    }
    Ok(LineageList {
        lineages: summaries,
        warnings,
    })
}

pub(crate) fn view(context: &Context, name: &str) -> AppResult<LineageView> {
    let lineage = require_lineage(context, name)?;
    let members = lineage.members(context)?;
    let catalog = voice_catalog(&lineage, InvalidGapPolicy::WarnAndHide)?;
    let terms = terms_document(&lineage)?;
    Ok(LineageView {
        lineage,
        members,
        voices: catalog.voices,
        withdrawn_voices: catalog.withdrawn_voices,
        terms: TermsView {
            present: terms.is_some(),
            text: terms.as_ref().map(|terms| terms.text.clone()),
            digest: terms.map(|terms| terms.digest),
        },
        warnings: catalog.warnings,
    })
}

pub(crate) fn render_voices(
    lineage: &Lineage,
    index: &[VoiceIndex],
    withdrawn_voices: usize,
) -> AppResult<Vec<String>> {
    let mut rendered = Vec::new();
    for voice in index {
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
    for _ in 0..withdrawn_voices {
        rendered.push("[post] one voice withdrawn".to_owned());
    }
    Ok(rendered)
}

pub(crate) fn create(
    context: &Context,
    acting: &Participant,
    name: &str,
) -> AppResult<CreateResult> {
    let _lock = participant::lock(context)?;
    lineage::validate_name(context, name)?;
    let mut acting = current_actor(context, acting)?;
    if let Some(existing) = lineage::load(context, name)? {
        if existing.founder == acting.id {
            if acting.lineage.as_deref() == Some(name) {
                return Ok(CreateResult::Affiliated(Box::new(Mutation {
                    value: (existing, acting),
                    changed: false,
                    warnings: Vec::new(),
                })));
            }
            if acting.lineage.is_none() {
                if let Some(terms) = terms_document(&existing)? {
                    return Ok(CreateResult::NeedsAcknowledgement {
                        lineage: existing,
                        terms,
                    });
                }
                let (_, at) = local_timestamp()?;
                acting.lineage = Some(name.to_owned());
                acting.lineage_since = Some(at.clone());
                write_participant(&acting)?;
                let warnings = append_warning(&existing.dir, &at, "new", &acting.id, None);
                return Ok(CreateResult::Affiliated(Box::new(Mutation {
                    value: (existing, acting),
                    changed: true,
                    warnings,
                })));
            }
            ensure_can_affiliate(&acting, name)?;
        }
        return Err(
            AppError::invalid_argument(format!("lineage '{name}' already exists"))
                .input(name)
                .reason("lineage already exists"),
        );
    }
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
    let warnings = append_warning(&dir, &created, "new", &acting.id, None);
    let value = (
        Lineage {
            version: LINEAGE_VERSION,
            name: name.to_owned(),
            founder: acting.id.clone(),
            created,
            host,
            dir,
        },
        acting,
    );
    Ok(CreateResult::Affiliated(Box::new(Mutation {
        value,
        changed: true,
        warnings,
    })))
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

    if acting.lineage.as_deref() == Some(name) {
        let terms = if acknowledge {
            terms_document(&lineage)?
        } else {
            None
        };
        return Ok(ContinueResult::Affiliated {
            lineage,
            participant: acting.id,
            changed: false,
            warnings: Vec::new(),
            terms,
        });
    }
    let terms = terms_document(&lineage)?;
    if let Some(terms) = terms {
        if acknowledge {
            let (_, at) = local_timestamp()?;
            acting.lineage = Some(name.to_owned());
            acting.lineage_since = Some(at.clone());
            write_participant(&acting)?;
            let warnings = append_warning(
                &lineage.dir,
                &at,
                "continue",
                &acting.id,
                Some(&terms.digest),
            );
            return Ok(ContinueResult::Affiliated {
                lineage,
                participant: acting.id,
                changed: true,
                warnings,
                terms: Some(terms),
            });
        }
        return Ok(ContinueResult::NeedsAcknowledgement { lineage, terms });
    }

    let (_, at) = local_timestamp()?;
    acting.lineage = Some(name.to_owned());
    acting.lineage_since = Some(at.clone());
    write_participant(&acting)?;
    let warnings = append_warning(&lineage.dir, &at, "continue", &acting.id, None);
    Ok(ContinueResult::Affiliated {
        lineage,
        participant: acting.id,
        changed: true,
        warnings,
        terms: None,
    })
}

pub(crate) fn leave(
    context: &Context,
    acting: &Participant,
) -> AppResult<Mutation<(Participant, Option<String>)>> {
    let _lock = participant::lock(context)?;
    let mut acting = current_actor(context, acting)?;
    let Some(name) = acting.lineage.clone() else {
        return Ok(Mutation {
            value: (acting, None),
            changed: false,
            warnings: Vec::new(),
        });
    };
    let (_, at) = local_timestamp()?;
    acting.lineage = None;
    acting.lineage_since = None;
    write_participant(&acting)?;
    let dir = context.root.join(LINEAGES_DIR).join(&name);
    let warnings = if dir.is_dir() {
        append_warning(&dir, &at, "leave", &acting.id, None)
    } else {
        Vec::new()
    };
    Ok(Mutation {
        value: (acting, Some(name)),
        changed: true,
        warnings,
    })
}

pub(crate) fn add_voice(
    context: &Context,
    acting: &Participant,
    author: &str,
    body_file: &Path,
) -> AppResult<Mutation<VoiceChange>> {
    ensure_own_voice(acting, author)?;
    let body = read_voice_input(body_file)?;
    let _lock = participant::lock(context)?;
    let acting = current_actor(context, acting)?;
    ensure_own_voice(&acting, author)?;
    let lineage = acting_lineage(context, &acting)?;
    let voices = lineage.dir.join(VOICES_DIR);
    fs::create_dir_all(&voices)
        .map_err(|error| AppError::io("create lineage voices directory", &voices, error))?;
    let cleanup = finish_pending_cleanup(&voices, author)?;
    let mut warnings = Vec::new();
    if cleanup.completed_pending {
        let (_, at) = local_timestamp()?;
        warnings.extend(append_warning(
            &lineage.dir,
            &at,
            "voice_withdraw",
            &acting.id,
            None,
        ));
    }

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
    let (_, at) = local_timestamp()?;
    warnings.extend(append_warning(
        &lineage.dir,
        &at,
        if revisions == 0 {
            "voice_add"
        } else {
            "voice_revise"
        },
        &acting.id,
        None,
    ));
    Ok(Mutation {
        value: VoiceChange {
            lineage: lineage.name,
            revisions: Some(revisions),
            hint: None,
        },
        changed: true,
        warnings,
    })
}

pub(crate) fn withdraw_voice(
    context: &Context,
    acting: &Participant,
    author: &str,
    requested_lineage: Option<&str>,
) -> AppResult<Mutation<VoiceChange>> {
    ensure_own_voice(acting, author)?;
    let _lock = participant::lock(context)?;
    let acting = current_actor(context, acting)?;
    ensure_own_voice(&acting, author)?;
    if let Some(name) = requested_lineage {
        let dir = voice_lineage_dir(context, name)?;
        return withdraw_selected_voice(&acting, name, &dir, author, false);
    }
    if let Some(name) = &acting.lineage {
        let dir = context.root.join(LINEAGES_DIR).join(name);
        let voices = dir.join(VOICES_DIR);
        if read_optional_gap(&voices, author)?.is_some()
            || voices.join(format!("{author}.md")).is_file()
        {
            return withdraw_selected_voice(&acting, name, &dir, author, true);
        }
    }

    let (name, dir) = resolve_voice_lineage(context, author)?;
    withdraw_selected_voice(&acting, &name, &dir, author, false)
}

fn withdraw_selected_voice(
    acting: &Participant,
    name: &str,
    dir: &Path,
    author: &str,
    current_lineage: bool,
) -> AppResult<Mutation<VoiceChange>> {
    let voices = dir.join(VOICES_DIR);
    let gap = read_optional_gap(&voices, author)?;
    if gap.is_some_and(|gap| gap.cleanup_pending) {
        let cleanup = finish_pending_cleanup(&voices, author)?;
        debug_assert!(cleanup.completed_pending);
        return finish_recovered_withdrawal(acting, name, dir);
    }
    if voices.join(format!("{author}.md")).is_file() {
        return withdraw_current_voice(acting, name, dir, author, gap);
    }
    if gap.is_some() {
        let hint = current_lineage.then(|| {
            "no current voice here; to withdraw a voice on another lineage, run: post identity voice withdraw --lineage NAME"
                .to_owned()
        });
        return Ok(Mutation {
            value: VoiceChange {
                lineage: name.to_owned(),
                revisions: None,
                hint,
            },
            changed: false,
            warnings: Vec::new(),
        });
    }
    Err(no_voice(author))
}

fn withdraw_current_voice(
    acting: &Participant,
    name: &str,
    dir: &Path,
    author: &str,
    prior_gap: Option<GapRecord>,
) -> AppResult<Mutation<VoiceChange>> {
    let voices = dir.join(VOICES_DIR);
    let current = voices.join(format!("{author}.md"));
    if !current.is_file() {
        return Err(no_voice(author));
    }
    let (_, at) = local_timestamp()?;
    let mut gap = prior_gap.unwrap_or(GapRecord {
        version: 1,
        withdrawals: 0,
        cleanup_pending: false,
    });
    gap.withdrawals = gap.withdrawals.checked_add(1).ok_or_else(|| {
        AppError::new(
            ErrorCode::IoError,
            "voice withdrawal count overflowed",
            "Preserve the lineage voice files and report this lineage for repair.",
        )
    })?;
    gap.cleanup_pending = true;
    let gap_path = voices.join(format!("{author}.gap"));
    write_json(&gap_path, &gap, "mark voice withdrawal cleanup pending")?;
    cleanup_voice_files(&voices, author)?;
    gap.cleanup_pending = false;
    write_json(&gap_path, &gap, "finish voice withdrawal cleanup")?;
    let warnings = append_warning(dir, &at, "voice_withdraw", &acting.id, None);
    Ok(Mutation {
        value: VoiceChange {
            lineage: name.to_owned(),
            revisions: None,
            hint: None,
        },
        changed: true,
        warnings,
    })
}

fn finish_recovered_withdrawal(
    acting: &Participant,
    name: &str,
    dir: &Path,
) -> AppResult<Mutation<VoiceChange>> {
    let (_, at) = local_timestamp()?;
    let warnings = append_warning(dir, &at, "voice_withdraw", &acting.id, None);
    Ok(Mutation {
        value: VoiceChange {
            lineage: name.to_owned(),
            revisions: None,
            hint: None,
        },
        changed: true,
        warnings,
    })
}

pub(crate) fn set_terms(
    context: &Context,
    acting: &Participant,
    body_file: &Path,
) -> AppResult<Mutation<VoiceChange>> {
    let body = read_content_input(body_file, "terms body")?;
    let _lock = participant::lock(context)?;
    let acting = current_actor(context, acting)?;
    let lineage = acting_lineage(context, &acting)?;
    let path = lineage.dir.join(TERMS_FILE);
    atomic_replace(&path, body.as_bytes())
        .map_err(|error| AppError::io("write lineage terms", &path, error))?;
    let (_, at) = local_timestamp()?;
    let warnings = append_warning(&lineage.dir, &at, "terms_set", &acting.id, None);
    Ok(Mutation {
        value: VoiceChange {
            lineage: lineage.name,
            revisions: None,
            hint: None,
        },
        changed: true,
        warnings,
    })
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

fn voice_lineage_dir(context: &Context, name: &str) -> AppResult<PathBuf> {
    validate_room_name(name).map_err(|reason| {
        AppError::invalid_argument(format!("lineage name '{name}' is invalid: {reason}"))
            .input(name)
            .reason(reason)
    })?;
    let dir = context.root.join(LINEAGES_DIR).join(name);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.is_dir() => Ok(dir),
        Ok(_) => Err(AppError::new(
            ErrorCode::NotFound,
            format!("lineage '{name}' was not found"),
            "Run `post identity list` and retry with an existing lineage name.",
        )
        .input(name)
        .reason("lineage directory is absent")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(AppError::new(
            ErrorCode::NotFound,
            format!("lineage '{name}' was not found"),
            "Run `post identity list` and retry with an existing lineage name.",
        )
        .input(name)
        .reason("lineage directory is absent")),
        Err(error) => Err(AppError::io("inspect lineage directory", &dir, error)),
    }
}

fn resolve_voice_lineage(
    context: &Context,
    author: &str,
) -> AppResult<(String, std::path::PathBuf)> {
    let root = context.root.join(LINEAGES_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(no_voice(author)),
        Err(error) => return Err(AppError::io("search lineage voices", &root, error)),
    };
    let mut matches = Vec::new();
    let mut settled_gaps = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read lineage entry", &root, error))?;
        if !entry
            .file_type()
            .map_err(|error| AppError::io("inspect lineage entry", &entry.path(), error))?
            .is_dir()
        {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let voices = entry.path().join(VOICES_DIR);
        let gap = read_optional_gap(&voices, author)?;
        let current = voices.join(format!("{author}.md")).is_file();
        let cleanup_pending = gap.is_some_and(|gap| gap.cleanup_pending);
        if current || cleanup_pending {
            matches.push((name, entry.path()));
        } else if gap.is_some() {
            settled_gaps.push((name, entry.path()));
        }
    }
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    settled_gaps.sort_by(|left, right| left.0.cmp(&right.0));
    if matches.is_empty() && settled_gaps.len() == 1 {
        return Ok(settled_gaps.pop().expect("one settled gap exists"));
    }
    match matches.len() {
        0 => Err(no_voice(author)),
        1 => Ok(matches.pop().expect("one voice match exists")),
        _ => {
            let names: Vec<String> = matches.into_iter().map(|(name, _)| name).collect();
            let commands = names
                .iter()
                .map(|name| {
                    format!(
                        "`post identity voice withdraw --lineage {}`",
                        shell_quote(name)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            Err(AppError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "participant '{author}' has voices in several lineages: {}; withdrawal is ambiguous",
                    names.join(", ")
                ),
                format!("Choose the intended lineage and run one of: {commands}."),
            )
            .matches(names)
            .reason("withdrawal requires exactly one matching current or pending voice"))
        }
    }
}

fn no_voice(author: &str) -> AppError {
    AppError::new(
        ErrorCode::NotFound,
        format!("participant '{author}' has no current voice to withdraw"),
        "Add your own voice first with `post identity voice add --body-file PATH`.",
    )
    .reason("the acting participant has no current lineage voice")
}

fn read_optional_gap(voices: &Path, author: &str) -> AppResult<Option<GapRecord>> {
    let path = voices.join(format!("{author}.gap"));
    let gap = match fs::metadata(&path) {
        Ok(_) => read_gap(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("inspect voice withdrawal gap", &path, error)),
    };
    Ok(Some(gap))
}

fn finish_pending_cleanup(voices: &Path, author: &str) -> AppResult<GapCleanup> {
    let path = voices.join(format!("{author}.gap"));
    let Some(mut gap) = read_optional_gap(voices, author)? else {
        return Ok(GapCleanup {
            completed_pending: false,
        });
    };
    let mut completed_pending = false;
    if gap.cleanup_pending {
        cleanup_voice_files(voices, author)?;
        gap.cleanup_pending = false;
        write_json(&path, &gap, "finish pending voice withdrawal cleanup")?;
        completed_pending = true;
    }
    Ok(GapCleanup { completed_pending })
}

fn cleanup_voice_files(voices: &Path, author: &str) -> AppResult<()> {
    remove_file_if_exists(
        &voices.join(format!("{author}.md")),
        "remove current lineage voice",
    )?;
    remove_dir_if_exists(
        &voices.join(format!("{author}.history")),
        "remove lineage voice history",
    )?;
    let prefix = format!(".{author}.md.");
    let entries = match fs::read_dir(voices) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::io("list voice cleanup files", voices, error)),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read voice cleanup entry", voices, error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with(&prefix) && name.ends_with(".tmp") {
            remove_file_if_exists(&entry.path(), "remove leftover voice temporary")?;
        }
    }
    Ok(())
}

fn voice_catalog(
    lineage: &Lineage,
    invalid_gap_policy: InvalidGapPolicy,
) -> AppResult<VoiceCatalog> {
    let root = lineage.dir.join(VOICES_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(VoiceCatalog {
                voices: Vec::new(),
                withdrawn_voices: 0,
                warnings: Vec::new(),
            })
        }
        Err(error) => return Err(AppError::io("list lineage voices", &root, error)),
    };
    let mut current = BTreeSet::new();
    let mut gaps = BTreeMap::new();
    let mut histories = BTreeMap::new();
    let mut invalid_authors = BTreeSet::new();
    let mut warnings = Vec::new();
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
                current.insert(author.to_owned());
            } else if let Some(author) = name.strip_suffix(".gap") {
                match read_gap(&entry.path()) {
                    Ok(gap) => {
                        gaps.insert(author.to_owned(), gap);
                    }
                    Err(error) => match invalid_gap_policy {
                        InvalidGapPolicy::Fail => return Err(error),
                        InvalidGapPolicy::WarnAndHide => {
                            invalid_authors.insert(author.to_owned());
                            warnings.push(format!(
                                "one voice withdrawal marker is invalid and its content was hidden: {}",
                                error
                                    .details
                                    .reason
                                    .as_deref()
                                    .unwrap_or("invalid gap marker")
                            ));
                        }
                    },
                }
            }
        } else if file_type.is_dir() {
            if let Some(author) = name.strip_suffix(".history") {
                histories.insert(author.to_owned(), history_revision_count(&entry.path())?);
            }
        }
    }
    let withdrawn_voices = gaps.values().map(|gap| gap.withdrawals).sum();
    let voices = current
        .into_iter()
        .filter(|participant| {
            !invalid_authors.contains(participant)
                && !gaps.get(participant).is_some_and(|gap| gap.cleanup_pending)
        })
        .map(|participant| VoiceIndex {
            revisions: histories.get(&participant).copied().unwrap_or(0),
            participant,
        })
        .collect();
    Ok(VoiceCatalog {
        voices,
        withdrawn_voices,
        warnings,
    })
}

fn read_gap(path: &Path) -> AppResult<GapRecord> {
    let bytes =
        fs::read(path).map_err(|error| AppError::io("read voice withdrawal gap", path, error))?;
    let gap: GapRecord = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(path, format!("invalid voice gap JSON: {error}")))?;
    if gap.version != 1 || gap.withdrawals == 0 {
        return Err(AppError::config(
            path,
            "voice gap must have version 1 and at least one withdrawal",
        ));
    }
    Ok(gap)
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
    read_content_input(path, "voice body")
}

fn read_content_input(path: &Path, label: &str) -> AppResult<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| AppError::io(&format!("open {label} file"), path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| AppError::io(&format!("inspect {label} file"), path, error))?;
    if !metadata.is_file() {
        return Err(AppError::invalid_argument(format!(
            "{label} '{}' must be a regular file",
            path.display()
        ))
        .path(path.display().to_string())
        .reason(format!(
            "{label} files must be regular files; symlinks are rejected"
        )));
    }
    if metadata.len() > VOICE_MAX_BYTES {
        return Err(AppError::invalid_argument(format!(
            "{label} '{}' is {} bytes; the maximum is {VOICE_MAX_BYTES}",
            path.display(),
            metadata.len()
        ))
        .path(path.display().to_string())
        .reason(format!("{label} exceeds the 4096-byte cap")));
    }
    read_content_from_file(file, metadata.len(), path, label)
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
    read_content_from_file(file, metadata.len(), path, "stored lineage voice")
}

fn read_content_from_file(
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

fn terms_document(lineage: &Lineage) -> AppResult<Option<TermsDocument>> {
    let path = lineage.dir.join(TERMS_FILE);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("open stored lineage terms", &path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| AppError::io("inspect stored lineage terms", &path, error))?;
    if !metadata.is_file() || metadata.len() > VOICE_MAX_BYTES {
        return Err(AppError::config(
            &path,
            "stored lineage terms are not a regular file within the 4096-byte cap",
        ));
    }
    let text = read_content_from_file(file, metadata.len(), &path, "stored lineage terms")?;
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    Ok(Some(TermsDocument { text, digest }))
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

fn append_warning(
    dir: &Path,
    at: &str,
    event: &str,
    participant: &str,
    terms_digest: Option<&str>,
) -> Vec<String> {
    append_event(dir, at, event, participant, terms_digest)
        .err()
        .map(|error| {
            vec![format!(
                "state committed, but lineage journal event '{event}' was not recorded: {}",
                error.message
            )]
        })
        .unwrap_or_default()
}

fn append_event(
    dir: &Path,
    at: &str,
    event: &str,
    participant: &str,
    terms_digest: Option<&str>,
) -> AppResult<()> {
    let path = dir.join(HISTORY_FILE);
    let mut bytes = serde_json::to_vec(&JournalEntry {
        at: at.to_owned(),
        event: event.to_owned(),
        participant: participant.to_owned(),
        terms_digest: terms_digest.map(str::to_owned),
    })
    .map_err(|error| AppError::io("serialize lineage journal entry", &path, error))?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| AppError::io("open lineage journal", &path, error))?;
    repair_torn_tail(&mut file, &path)?;
    file.seek(SeekFrom::End(0))
        .map_err(|error| AppError::io("seek lineage journal", &path, error))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| AppError::io("append and sync lineage journal", &path, error))
}

fn repair_torn_tail(file: &mut File, path: &Path) -> AppResult<()> {
    let length = file
        .metadata()
        .map_err(|error| AppError::io("inspect lineage journal", path, error))?
        .len();
    if length == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))
        .map_err(|error| AppError::io("seek lineage journal tail", path, error))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)
        .map_err(|error| AppError::io("read lineage journal tail", path, error))?;
    if last[0] == b'\n' {
        return Ok(());
    }

    const CHUNK: u64 = 8 * 1024;
    let mut end = length;
    let mut buffer = [0_u8; CHUNK as usize];
    loop {
        let start = end.saturating_sub(CHUNK);
        let size = (end - start) as usize;
        file.seek(SeekFrom::Start(start))
            .map_err(|error| AppError::io("seek lineage journal tail", path, error))?;
        file.read_exact(&mut buffer[..size])
            .map_err(|error| AppError::io("scan lineage journal tail", path, error))?;
        if let Some(index) = buffer[..size].iter().rposition(|byte| *byte == b'\n') {
            return file
                .set_len(start + index as u64 + 1)
                .map_err(|error| AppError::io("truncate torn lineage journal tail", path, error));
        }
        if start == 0 {
            return file
                .set_len(0)
                .map_err(|error| AppError::io("truncate torn lineage journal tail", path, error));
        }
        end = start;
    }
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
            version: 1,
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
