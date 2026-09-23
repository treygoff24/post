//! Channel store: shared append-only group-chat history.
//!
//! Contract: mail 20260722-013246 (pinned), amended by 013434 (microsecond
//! ids). A channel is NOT a room and its messages are NOT mail: no kind
//! field (the kinds law stays untouched), no per-recipient copies, no
//! archive/ — messages/ is itself the immutable record because nothing is
//! ever moved or deleted from it. Membership is explicit and open to any
//! registered room; joins are recorded both in members.json (the index)
//! and as an event message in the history (the record). Blocked routes
//! bar shared membership at join time; channels never carry what a route
//! may not.

use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{
    ascii_escape_json, atomic_replace, exclusive_atomic_write, local_timestamp_micros, new_mail_id,
    validate_room_name, Context,
};
use crate::model::{ChannelMessage, ParsedChannelMessage, RoomMap, SenderProvenance};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(crate) const CHANNELS_DIR: &str = "channels";
const CHANNELS_LOCK_FILE: &str = ".channels.lock";
pub(crate) const JOIN_EVENT: &str = "join";
/// Profile-change announcement ("=== pact is now Lantern 🏮 (pact) ===").
pub(crate) const PROFILE_EVENT: &str = "profile";

pub(crate) type MemberMap = BTreeMap<String, String>;

#[allow(dead_code)] // consumed by the read/cursor + doctor lanes' patches
pub(crate) fn channel_state_path(context: &Context, room: &str) -> AppResult<PathBuf> {
    validate_room_name(room).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room '{room}' is invalid: {reason}"),
            "Pass a single room name without path separators.",
        )
    })?;
    Ok(context.root.join(room).join("channel-state.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    pub created: String,
    pub created_by: String,
    /// Norms carrier for the channel; any member may update via
    /// `--join --description`. Cap 1 KiB. Absent on pre-description stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ChannelPaths {
    #[allow(dead_code)] // consumed by the doctor lane's patch
    pub dir: PathBuf,
    pub messages: PathBuf,
    pub channel_json: PathBuf,
    pub members_json: PathBuf,
}

impl ChannelPaths {
    pub(crate) fn new(context: &Context, channel: &str) -> AppResult<Self> {
        validate_channel_name(channel)?;
        let dir = context.root.join(CHANNELS_DIR).join(channel);
        Ok(Self {
            messages: dir.join("messages"),
            channel_json: dir.join("channel.json"),
            members_json: dir.join("members.json"),
            dir,
        })
    }

    pub(crate) fn exists(&self) -> bool {
        self.channel_json.is_file()
    }

    pub(crate) fn load_members(&self) -> AppResult<MemberMap> {
        let bytes = match fs::read(&self.members_json) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MemberMap::new());
            }
            Err(error) => {
                return Err(AppError::io(
                    "read channel members",
                    &self.members_json,
                    error,
                ))
            }
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            AppError::config(&self.members_json, format!("invalid JSON object: {error}"))
        })
    }

    pub(crate) fn load_info(&self) -> AppResult<ChannelInfo> {
        let bytes = fs::read(&self.channel_json)
            .map_err(|error| AppError::io("read channel info", &self.channel_json, error))?;
        serde_json::from_slice(&bytes).map_err(|error| {
            AppError::config(&self.channel_json, format!("invalid JSON object: {error}"))
        })
    }
}

pub(crate) fn validate_channel_name(value: &str) -> AppResult<()> {
    validate_room_name(value).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("channel '{value}' is invalid: {reason}"),
            "Pass a single path-safe channel name without '/' or '\\'.",
        )
        .input(value)
        .reason(reason)
    })
}

/// One lock for all membership mutation across every channel: joins are
/// rare and human-paced, so a global lock is simpler than per-channel
/// locks and cannot deadlock. Message sends never take it — exclusive
/// file creation is their arbiter.
pub(crate) fn lock_channels(context: &Context) -> AppResult<File> {
    let dir = context.root.join(CHANNELS_DIR);
    fs::create_dir_all(&dir)
        .map_err(|error| AppError::io("create channels directory", &dir, error))?;
    let path = dir.join(CHANNELS_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| AppError::io("open channels lock", &path, error))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(AppError::io(
            "lock channels registry",
            &path,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(file)
}

/// Resolve the acting room for channel operations. A bound participant's
/// workspace is authoritative; unbound read-only commands retain the legacy
/// POST_FROM-or-cwd lookup. There is deliberately no --from/--room override
/// on channel commands. Membership additionally requires a registered room.
pub(crate) fn acting_room(
    context: &Context,
    rooms: &RoomMap,
) -> AppResult<(String, SenderProvenance)> {
    let (room, provenance) = match crate::participant::resolve(context) {
        Ok(crate::participant::Resolved::Bound { participant, .. }) => {
            let room = participant
                .workspace
                .clone()
                .unwrap_or_else(|| participant.id.clone());
            let provenance = match crate::mailbox::declared_env_pin()? {
                Some(pin) => {
                    if pin != room {
                        return Err(AppError::new(
                            ErrorCode::InvalidArgument,
                            format!(
                                "POST_FROM workspace pin '{pin}' conflicts with bound participant '{}' reply address '{room}'",
                                participant.id
                            ),
                            "Run `post participant bind --workspace <room>` to change workspace context deliberately.",
                        )
                        .input(pin)
                        .reason("workspace pin disagrees with bound participant"));
                    }
                    SenderProvenance::DeclaredEnv
                }
                None => SenderProvenance::ParticipantBinding,
            };
            (room, provenance)
        }
        Ok(crate::participant::Resolved::Unbound) => {
            context.resolved_room_with_provenance(None, rooms)?
        }
        Err(_) if crate::mailbox::read_only_command() => {
            context.resolved_room_with_provenance(None, rooms)?
        }
        Err(error) => return Err(error),
    };
    // A participant without workspace context is still a first-class channel
    // actor; its participant id is the shared reply address. Legacy code
    // required a registered room here, which made session-only participants
    // unable to join despite having durable membership/read state.
    Ok((room, provenance))
}

pub(crate) struct JoinOutcome {
    pub room: String,
    pub channel_created: bool,
    pub already_member: bool,
    pub event_id: Option<String>,
    /// Messages that predated this join and now read as history. Absent on
    /// an already-member response, which records no new membership start.
    pub history_before_join: Option<usize>,
}

pub(crate) fn join(
    context: &Context,
    channel: &str,
    description: Option<&str>,
    backlog: bool,
) -> AppResult<JoinOutcome> {
    let rooms = context.load_rooms()?;
    let (room, provenance) = acting_room(context, &rooms)?;
    let actor = context.sender()?;
    let paths = ChannelPaths::new(context, channel)?;
    let _lock = lock_channels(context)?;

    if let Some(description) = description {
        validate_description(description)?;
    }

    let membership = crate::channel_state::ParticipantChannels::load(&actor.participant)?;
    if membership.joined_names().contains(channel) {
        // Already a member: --description still updates the norms carrier.
        if let Some(description) = description {
            write_description(&paths, description)?;
        }
        return Ok(JoinOutcome {
            room,
            channel_created: false,
            already_member: true,
            event_id: None,
            history_before_join: None,
        });
    }

    // Blocked routes bar shared membership, both directions, before any
    // state is written. Checked under the lock so a concurrent join of the
    // blocked counterpart cannot slip in between check and write.
    let rules = context.load_rules(&rooms)?;
    let mut existing_members: Vec<(String, String)> =
        crate::channel_state::participants_for_join_validation(
            context,
            channel,
            &actor.participant,
            &room,
            &rules.blocked,
        )?
        .into_iter()
        .map(|member| {
            let address = member
                .workspace
                .clone()
                .unwrap_or_else(|| member.id.clone());
            (member.id, address)
        })
        .collect();
    for workspace in paths.load_members()?.into_keys() {
        if !existing_members
            .iter()
            .any(|(_, address)| address == &workspace)
        {
            existing_members.push((workspace.clone(), workspace));
        }
    }
    for (member_id, member_address) in existing_members {
        if let Some(rule) = rules.blocked.iter().find(|rule| {
            rule.matches_route(&room, &member_address)
                || rule.matches_route(&member_address, &room)
                || rule.matches_route(&actor.participant.id, &member_id)
                || rule.matches_route(&member_id, &actor.participant.id)
        }) {
            return Err(AppError::new(
                ErrorCode::BlockedRoute,
                format!(
                    "joining '{channel}' would put '{room}' and existing member '{member}' in one channel, and that route is blocked: {}",
                    rule.reason,
                    member = member_id
                ),
                "Do not route around this block. Ask the human operator to review rules.json.",
            )
            .input(format!("{} <-> {}", actor.participant.id, member_id))
            .reason(rule.reason.clone())
            .rule(rule.clone()));
        }
    }

    fs::create_dir_all(&paths.messages).map_err(|error| {
        AppError::io("create channel messages directory", &paths.messages, error)
    })?;

    // The membership start: join-from-now records this instant, so the whole
    // pre-join backlog is history rather than unread; `--backlog` records the
    // minimum watermark instead, keeping every message unread (the old
    // behavior, as an opt-in). The count mirrors exactly what the new member
    // will not see as unread: existing ids that sort below the start.
    let start = if backlog {
        crate::channel_state::BACKLOG_MEMBERSHIP_START.to_owned()
    } else {
        local_timestamp_micros()?.0
    };
    let history_before_join = message_files(&paths.messages)?
        .iter()
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|id| id < start.as_str())
        })
        .count();

    let (_, sent) = local_timestamp_micros()?;
    let channel_created = if paths.exists() {
        if let Some(description) = description {
            write_description(&paths, description)?;
        }
        false
    } else {
        let info = ChannelInfo {
            name: channel.to_owned(),
            created: sent.clone(),
            created_by: room.clone(),
            description: description
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        };
        write_channel_info(&paths, &info)?;
        true
    };

    // Event first, membership second: a crash between the two leaves a
    // visible join event without membership, which the next join attempt
    // repairs (at worst a duplicate event). The other order leaves a
    // member whose join the history never shows — a permanent hole.
    let event_id = write_message(
        context,
        &paths,
        WriteMessage {
            room: &room,
            channel,
            subject: "",
            body: &format!("=== {room} joined ==="),
            event: Some(JOIN_EVENT),
            re: None,
            mentions: Vec::new(),
            signature_tag: None,
            provenance,
        },
    )?;
    crate::channel_state::ParticipantChannels::join_at(
        context,
        &actor.participant,
        channel,
        &start,
    )?;

    Ok(JoinOutcome {
        room,
        channel_created,
        already_member: false,
        event_id: Some(event_id),
        history_before_join: Some(history_before_join),
    })
}

const MAX_DESCRIPTION_BYTES: usize = 1024;

pub(crate) fn validate_description(description: &str) -> AppResult<()> {
    if description.len() <= MAX_DESCRIPTION_BYTES {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "channel description is {} bytes; the maximum is {MAX_DESCRIPTION_BYTES} bytes",
            description.len()
        ),
        "Keep --description at or below 1024 bytes (same cap as subjects).",
    )
    .input("--description")
    .reason(format!(
        "description exceeds {MAX_DESCRIPTION_BYTES}-byte safety limit"
    )))
}

fn write_channel_info(paths: &ChannelPaths, info: &ChannelInfo) -> AppResult<()> {
    let mut bytes = serde_json::to_vec_pretty(info).map_err(|error| {
        AppError::io(
            "serialize channel info",
            &paths.channel_json,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        )
    })?;
    bytes.push(b'\n');
    atomic_replace(&paths.channel_json, &bytes)
        .map_err(|error| AppError::io("write channel info", &paths.channel_json, error))
}

fn write_description(paths: &ChannelPaths, description: &str) -> AppResult<()> {
    let mut info = paths.load_info()?;
    info.description = if description.is_empty() {
        None
    } else {
        Some(description.to_owned())
    };
    write_channel_info(paths, &info)
}

pub(crate) struct SendOptions<'a> {
    pub subject: &'a str,
    pub body: &'a str,
    /// How to re-supply this body on a retry (` --body '...'` / ` --body-file
    /// '...'`), or empty when it arrived on stdin and no command can carry it.
    /// crossed_send's exact_fix appends it so the refusal hands back the
    /// caller's own send, not a send with the message missing.
    pub body_flag: &'a str,
    pub anyway: bool,
    pub re: Option<&'a str>,
    /// Signed-v2 sidecar tag; when present the envelope is stamped with the
    /// exact locator `{"version": 2, "tag": <tag>}`. Validated at the CLI.
    pub signature_tag: Option<&'a str>,
}

pub(crate) fn send(
    context: &Context,
    channel: &str,
    options: SendOptions<'_>,
) -> AppResult<ChannelMessage> {
    let rooms = context.load_rooms()?;
    let (room, provenance) = acting_room(context, &rooms)?;
    let actor = context.sender()?;
    let paths = ChannelPaths::new(context, channel)?;
    let quoted = crate::mailbox::shell_quote(channel);
    if !paths.exists() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("channel '{channel}' does not exist"),
            format!("Create it with `post chat {quoted} --join`, then retry the send."),
        )
        .input(channel)
        .reason("no channel.json under the channels directory"));
    }
    let membership = crate::channel_state::ParticipantChannels::load(&actor.participant)?;
    if !membership.effective(context, &actor.participant, channel)? {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            format!(
                "participant '{}' is not a member of channel '{channel}'",
                actor.participant.id
            ),
            format!("Join first with `post chat {quoted} --join`, then retry the send."),
        )
        .input(actor.participant.id)
        .reason(
            "participant is neither explicitly joined nor covered by a legacy workspace default",
        ));
    }
    if options.body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!(
                "Retry with `post chat {quoted} --send --body '<text>'` or a non-empty FILE/stdin."
            ),
        )
        .input("message body")
        .reason("empty or whitespace-only"));
    }

    // Crossed-send bounce: humans see incoming while typing; agents get the
    // equivalent at the send point. Check-then-append has a TOCTOU window
    // (another room can land a message between check and exclusive create);
    // that occasional slip is accepted. Corrupting the store is not.
    let crossed = crossed_send_check(
        context,
        &paths,
        channel,
        &room,
        &actor.participant,
        options.body_flag,
    )?;
    let (unseen, targeted) = (crossed.unseen, crossed.targeted);
    if options.anyway {
        log_crossed_event(
            context,
            channel,
            &room,
            unseen,
            targeted,
            CrossedOutcome::Anyway,
        );
    } else {
        match crossed.verdict {
            CrossedVerdict::Refuse(error) => {
                log_crossed_event(
                    context,
                    channel,
                    &room,
                    unseen,
                    targeted,
                    CrossedOutcome::Refused,
                );
                return Err(error);
            }
            CrossedVerdict::Warn => {
                // Deliver, but say what was crossed. Refusing here was the old
                // behaviour and it refused on any unseen message from anyone,
                // so a room that had just joined a busy channel was maximally
                // crossed by construction with nothing addressed to it.
                eprintln!(
                    "post: warning -- {unseen} unseen message(s) from others in #{channel}, none addressed to {room}; delivering anyway. Catch up with `post chat {}`.",
                    crate::mailbox::shell_quote(channel)
                );
                log_crossed_event(
                    context,
                    channel,
                    &room,
                    unseen,
                    targeted,
                    CrossedOutcome::Warned,
                );
            }
            CrossedVerdict::Clear => {}
        }
    }

    let re = match options.re {
        Some(prefix) => Some(resolve_message_id(&paths, prefix)?),
        None => None,
    };
    let mentions = extract_mentions(options.body, &rooms);
    let id = write_message(
        context,
        &paths,
        WriteMessage {
            room: &room,
            channel,
            subject: options.subject,
            body: options.body,
            event: None,
            re,
            mentions,
            signature_tag: options.signature_tag,
            provenance,
        },
    )?;
    let file = paths.messages.join(format!("{id}.msg"));
    Ok(parse_channel_message(&file)?.message)
}

/// What the unseen tip means for this send.
pub(crate) enum CrossedVerdict {
    /// Something unseen is addressed to this room: refuse.
    Refuse(AppError),
    /// Unseen messages exist but none concern this room: deliver, and say so.
    Warn,
    /// Nothing unseen from others.
    Clear,
}

pub(crate) struct CrossedReport {
    pub verdict: CrossedVerdict,
    pub unseen: usize,
    pub targeted: usize,
}

pub(crate) enum CrossedOutcome {
    Refused,
    Warned,
    Anyway,
}

impl CrossedOutcome {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Warned => "warned",
            Self::Anyway => "anyway",
        }
    }
}

/// Decide whether the unseen channel tip should stop this send.
///
/// It used to stop every send with any unseen message from anyone, which fired
/// hardest in the situation where it protected least: a room that has just
/// joined a busy channel is maximally crossed by construction, and none of those
/// hundreds of messages are addressed to it. Agents learned to type `--anyway`
/// reflexively, so the guard was measuring its own bypass rate and nothing else.
///
/// The line is now the one this store already draws everywhere else: a message
/// that @mentions you, or replies to something you wrote, is conversation you
/// must not miss (`post chat` already refuses to silently drop mentions of the
/// reader from a skipped range). Everything else is conversation you may skim,
/// so it warns and delivers.
///
/// A malformed `.msg` the room already consumed is ignored; an unreadable UNSEEN
/// file refuses, because a message that cannot be parsed cannot be shown not to
/// concern you. `--anyway` remains the escape hatch for all of it.
fn crossed_send_check(
    context: &Context,
    paths: &ChannelPaths,
    channel: &str,
    room: &str,
    participant: &crate::participant::Participant,
    body_flag: &str,
) -> AppResult<CrossedReport> {
    use crate::error::MissedChannelMessage;

    // The parsed message is kept alongside the bounce payload so badge
    // computation (below) never needs a second parse of the store.
    struct MissedItem {
        bounce: MissedChannelMessage,
        message: crate::model::ChannelMessage,
        body: String,
        targeted: bool,
    }

    // Resolved once, before the scan: needed to decide targeting, and the same
    // value the badge pass below uses.
    let owner_room = crate::mailbox::resolve_owner(context)?.map(|owner| owner.room);
    let mut missed = Vec::new();
    let eligible = match crate::cursor_state::eligibility::unread_channel(
        context,
        participant,
        channel,
    ) {
        Ok(eligible) => eligible,
        Err(error) if error.code == ErrorCode::ConfigInvalid => {
            let fix = format!(
                "post chat {} --send --anyway{}",
                crate::mailbox::shell_quote(channel),
                body_flag
            );
            return Ok(CrossedReport {
                unseen: 1,
                targeted: 1,
                verdict: CrossedVerdict::Refuse(
                    AppError::new(
                        ErrorCode::CrossedSend,
                        format!(
                            "channel '{channel}' has unreadable unseen message(s); send was not delivered"
                        ),
                        format!(
                            "Inspect/repair the channel store, catch up with `post chat {}`, then revise; or retry with `--anyway` to deliver regardless: `{fix}`.",
                            crate::mailbox::shell_quote(channel)
                        ),
                    )
                    .exact_fix(fix)
                    .input(channel)
                    .reason("unreadable unseen message"),
                ),
            });
        }
        Err(error) => return Err(error),
    };
    for item in eligible {
        // System events (join/profile) are not conversation the sender needs
        // to revise against; bounce only on ordinary messages from others.
        if item.message.event.is_some() {
            continue;
        }
        let message = item.message;
        // Addressed to this room: an @mention of it, or a reply to something it
        // wrote. `re` carries a message id, so the author of the parent has to
        // be looked up; only messages that actually carry one pay for that.
        // Addressed to this room: an @mention of it, a reply to something it
        // wrote, or a message from the owner room.
        //
        // The owner clause is not deference, it is a consequence: a signed owner
        // message binds its whole body, so adding "@you" to one invalidates the
        // signature. Without this clause an owner's signed word could never be
        // targeted, could therefore never appear in a refusal preview, and the
        // signed_verified badge on that path would become unreachable code. The
        // owner's messages to a channel are not skimmable conversation.
        let targeted = message.mentions.iter().any(|name| name == room)
            || owner_room.as_deref() == Some(message.from.as_str())
            || message
                .re
                .as_deref()
                .is_some_and(|parent| message_author_is(paths, parent, room));
        missed.push(MissedItem {
            targeted,
            bounce: MissedChannelMessage {
                id: message.id.clone(),
                from: message.from.clone(),
                subject: message.subject.clone(),
                sent: message.sent.clone(),
                body: item.body.clone(),
                signed_verified: None,
                sender_address: message.sender_address.clone(),
                sender_provenance: message.sender_provenance.clone(),
            },
            message,
            body: item.body,
        });
    }
    let unseen = missed.len();
    let targeted_count = missed.iter().filter(|item| item.targeted).count();
    if missed.is_empty() {
        return Ok(CrossedReport {
            verdict: CrossedVerdict::Clear,
            unseen: 0,
            targeted: 0,
        });
    }
    // Nothing here concerns this room, so delivering is the right default and
    // the caller says what was crossed rather than refusing over it.
    if targeted_count == 0 {
        return Ok(CrossedReport {
            verdict: CrossedVerdict::Warn,
            unseen,
            targeted: 0,
        });
    }
    // From here the send is refused, so only the messages that caused the
    // refusal belong in the preview. The old bounce embedded the last ten full
    // bodies -- roughly 15KB of prose, most of it already read -- which cost the
    // reader more context than the operation it refused (pc2_0dfb29556dec7b0c).
    missed.retain(|item| item.targeted);
    // Runnable as written: --anyway re-reads the body from stdin, so a caller
    // who was piping a heredoc keeps piping it. The old fix said
    // `--body '<revised text>'`, which is not a command -- and it steered the
    // caller onto argv, the one form this binary's own help calls dangerous
    // because the shell parses it first.
    let fix = format!(
        "post chat {} --send --anyway{}",
        crate::mailbox::shell_quote(channel),
        body_flag
    );
    // A bounce that renders missed conversation is a badge-computing surface
    // (A0a Decision 3): resolve the owner ONCE — a broken owner.json fails
    // the send with the config error instead of a crossed_send, and since
    // nothing has been written yet, the draft is preserved by construction.
    let owner = crate::mailbox::resolve_owner(context)?;
    if let Some(owner) = owner.as_ref() {
        for item in &mut missed {
            if item.message.from == owner.room {
                // Map, exactly like chat: signed-looking messages carry
                // Some(verified-bool), ordinary unsigned messages stay
                // None -> field omitted in JSON (A0a Decision 3).
                item.bounce.signed_verified =
                    crate::mailbox::signed_status(Some(owner), &item.message, &item.body, channel)
                        .map(|status| {
                            matches!(status, crate::mailbox::SignedStatus::Verified { .. })
                        });
            }
        }
    }
    let mut missed: Vec<MissedChannelMessage> = missed
        .into_iter()
        .map(|mut item| {
            // First line only. The whole body was never what the reader needed
            // to decide whether to revise, and the ids are right there.
            item.bounce.body = first_line(&item.bounce.body);
            item.bounce
        })
        .collect();
    let total = missed.len();
    if missed.len() > PREVIEW_CAP {
        missed = missed.split_off(missed.len() - PREVIEW_CAP);
    }
    let message = format!(
        "channel '{channel}' has {total} unseen message(s) addressed to '{room}' out of {unseen} unseen; send was not delivered (showing the last {}, first line only)",
        missed.len()
    );
    Ok(CrossedReport {
        verdict: CrossedVerdict::Refuse(
            AppError::new(ErrorCode::CrossedSend, message, format!(
                "Read the messages addressed to you, revise, then resend the same way you sent it -- body on stdin -- adding `--anyway`: `{fix}`."
            ))
            .exact_fix(fix)
            .input(channel)
            .reason("unseen messages are addressed to this room")
            .missed(missed),
        ),
        unseen,
        targeted: targeted_count,
    })
}

/// The crossed-send audit log: one JSON line per send that met an unseen tip.
///
/// This exists because the guard kept no record of itself. Asked for one real
/// interleaving the old refusal had prevented, nobody could produce one -- not
/// because none existed, but because a refusal is an error and errors are not
/// written anywhere. Weeks of running and zero evidence in either direction,
/// which meant the guard could only ever be tuned by argument. `anyway_after_ms`
/// is the field that settles it: the gap between a refusal and the `--anyway`
/// that followed measures whether anyone read what they were shown.
///
/// Best-effort by construction. Telemetry must never be able to fail a send.
fn log_crossed_event(
    context: &Context,
    channel: &str,
    room: &str,
    unseen: usize,
    targeted: usize,
    outcome: CrossedOutcome,
) {
    let path = context.root.join("crossed-send.jsonl");
    let Some(now_ms) = epoch_millis() else { return };
    let anyway_after_ms = match outcome {
        CrossedOutcome::Anyway => last_refusal_ms(&path, channel, room).map(|then| now_ms - then),
        _ => None,
    };
    let mut line = format!(
        "{{\"epoch_ms\":{now_ms},\"room\":\"{}\",\"channel\":\"{}\",\"unseen\":{unseen},\"targeted\":{targeted},\"outcome\":\"{}\"",
        ascii_escape_json(room),
        ascii_escape_json(channel),
        outcome.as_str()
    );
    if let Some(gap) = anyway_after_ms {
        line.push_str(&format!(",\"anyway_after_ms\":{gap}"));
    }
    line.push_str("}\n");
    // Telemetry must never be able to fail a send, so every error here is
    // deliberately dropped: a lost audit line costs a data point, a failed send
    // costs the message.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
}

fn epoch_millis() -> Option<u128> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis())
}

/// Epoch millis of this room's most recent refusal on this channel.
fn last_refusal_ms(path: &Path, channel: &str, room: &str) -> Option<u128> {
    let text = fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .filter(|line| line.contains(&format!("\"room\":\"{room}\"")))
        .filter(|line| line.contains(&format!("\"channel\":\"{channel}\"")))
        .find(|line| line.contains("\"outcome\":\"refused\""))
        .and_then(|line| {
            let rest = line.split("\"epoch_ms\":").nth(1)?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
}

/// How many targeted messages a refusal previews before summarizing.
const PREVIEW_CAP: usize = 5;

fn first_line(body: &str) -> String {
    const LINE_CAP: usize = 200;
    let line = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(LINE_CAP).collect();
    if line.chars().count() > LINE_CAP {
        out.push('\u{2026}');
    }
    out
}

/// Was `id` written by `room`? Used only to decide whether a reply is aimed here.
fn message_author_is(paths: &ChannelPaths, id: &str, room: &str) -> bool {
    let path = paths.messages.join(format!("{id}.msg"));
    parse_channel_message(&path).is_ok_and(|parsed| parsed.message.from == room)
}

/// Resolve a full id or unique prefix within this channel's messages/.
/// Only accepts paths that parse as channel messages whose envelope id matches
/// the filename stem — never trust a bare `.msg` name alone.
pub(crate) fn resolve_message_id(paths: &ChannelPaths, prefix: &str) -> AppResult<String> {
    // A reference printed by `post chat` carries a truncation mark when it is a
    // prefix; strip it so the token post printed resolves when pasted back.
    let prefix = crate::output::unmark_reference(prefix);
    let mut matches = Vec::new();
    for path in message_files(&paths.messages)? {
        let Ok(parsed) = parse_channel_message(&path) else {
            continue;
        };
        let id = parsed.message.id;
        if id == prefix || id.starts_with(prefix) {
            matches.push(id);
        }
    }
    match matches.len() {
        0 => Err(AppError::new(
            ErrorCode::NotFound,
            format!("no message in channel matching id/prefix '{prefix}'"),
            "Pass a full message id or a unique prefix from `post chat <channel> --history`.",
        )
        .input(prefix)
        .reason("no matching message id")),
        1 => Ok(matches.pop().expect("len 1")),
        _ => {
            matches.sort();
            Err(AppError::new(
                ErrorCode::AmbiguousId,
                format!(
                    "message id/prefix '{prefix}' matches {} messages",
                    matches.len()
                ),
                "Pass a longer unique prefix.",
            )
            .input(prefix)
            .matches(matches)
            .reason("ambiguous message id prefix"))
        }
    }
}

/// Word-boundary `@<room>` matches against registered room names. One
/// longest registered-name match per `@` occurrence so prefix pairs like
/// `foo`/`foo.bar` do not double-stamp. Matching is case-sensitive to the
/// registered spelling.
pub(crate) fn extract_mentions(body: &str, rooms: &RoomMap) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut names: Vec<&String> = rooms.keys().collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let mut found = BTreeSet::new();
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find('@') {
        let at = search_from + rel;
        if at > 0 {
            let before = body[..at].chars().next_back().unwrap_or('\0');
            if is_mention_boundary_char(before) {
                search_from = at + 1;
                continue;
            }
        }
        let after_at = at + '@'.len_utf8();
        let mut matched: Option<&String> = None;
        for name in &names {
            if !body[after_at..].starts_with(name.as_str()) {
                continue;
            }
            let end = after_at + name.len();
            let ok_end = body[end..]
                .chars()
                .next()
                .is_none_or(|c| !is_mention_boundary_char(c));
            if ok_end {
                matched = Some(name);
                break;
            }
        }
        if let Some(name) = matched {
            found.insert(name.clone());
            // Resume past the whole matched name: a registered name that
            // itself contains `@` (legal) must not re-trigger on its embedded
            // `@` and double-stamp the same token.
            search_from = after_at + name.len();
        } else {
            search_from = at + 1;
        }
    }
    found.into_iter().collect()
}

fn is_mention_boundary_char(c: char) -> bool {
    // Unicode-aware: ASCII-only would treat `é` as a boundary and stamp
    // `@foo` out of `@fooé`. Keep `_`/`-` as name continuations for the
    // registered room-name alphabet.
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// System-line writer for non-join events (currently profile changes).
/// Body is CLI-composed, never user text.
pub(crate) fn write_event(
    context: &Context,
    paths: &ChannelPaths,
    room: &str,
    channel: &str,
    body: &str,
    event: &str,
    provenance: SenderProvenance,
) -> AppResult<String> {
    write_message(
        context,
        paths,
        WriteMessage {
            room,
            channel,
            subject: "",
            body,
            event: Some(event),
            re: None,
            mentions: Vec::new(),
            signature_tag: None,
            provenance,
        },
    )
}

struct WriteMessage<'a> {
    room: &'a str,
    channel: &'a str,
    subject: &'a str,
    body: &'a str,
    event: Option<&'a str>,
    re: Option<String>,
    mentions: Vec<String>,
    signature_tag: Option<&'a str>,
    /// How `room` was resolved; stamped as evidence on the envelope.
    provenance: SenderProvenance,
}

/// Writes one message with a fresh microsecond-resolution id, retrying on
/// the (astronomically rare) same-microsecond hash collision.
fn write_message(
    context: &Context,
    paths: &ChannelPaths,
    opts: WriteMessage<'_>,
) -> AppResult<String> {
    // Send-time stamping: history renders names as they were when the
    // message was sent; renames never rewrite the transcript. Registry
    // values are re-validated at stamp time so a hand-edited profiles.json
    // is inert as an injection path.
    let rooms = context.load_rooms()?;
    let sender_address = crate::mailbox::declared_sender_address()?;
    let actor = context.sender()?;
    let profile = crate::profile::stamp_for(context, &actor.participant.id, opts.room, &rooms);
    if actor.from != opts.room {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "channel sender '{}' disagrees with bound participant '{}' reply address '{}'",
                opts.room, actor.participant.id, actor.from
            ),
            "Re-bind with the intended workspace before writing to a channel.",
        ));
    }
    for attempt in 0..256 {
        let (id_timestamp, sent) = local_timestamp_micros()?;
        let id = new_mail_id(&id_timestamp, attempt)?;
        let message = ChannelMessage {
            id: id.clone(),
            from: opts.room.to_owned(),
            channel: opts.channel.to_owned(),
            subject: opts.subject.to_owned(),
            sent,
            from_participant: Some(actor.participant.id.clone()),
            from_lineage: actor.lineage.clone(),
            address_kind: Some("channel".to_owned()),
            event: opts.event.map(str::to_owned),
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
            re: opts.re.clone(),
            mentions: opts.mentions.clone(),
            signature_ref: opts
                .signature_tag
                .map(|tag| serde_json::json!({ "version": 2, "tag": tag })),
            sender_address: sender_address.clone(),
            sender_provenance: Some(opts.provenance.as_str().to_owned()),
        };
        validate_channel_message(Path::new("<generated message>"), &message)?;
        let payload = encode_message(&message, opts.body)?;
        let path = paths.messages.join(format!("{id}.msg"));
        match exclusive_atomic_write(&path, &payload) {
            Ok(()) => return Ok(id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::io(
                    "exclusively write channel message",
                    &path,
                    error,
                ));
            }
        }
    }
    Err(AppError::new(
        ErrorCode::IoError,
        "could not allocate a unique channel message id after 256 attempts",
        "Retry the same command; if this repeats, run `post doctor`.",
    ))
}

pub(crate) fn encode_message(message: &ChannelMessage, body: &str) -> AppResult<Vec<u8>> {
    let payload = serde_json::to_string_pretty(message).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!(
                "failed to serialize channel message '{}': {error}",
                message.id
            ),
            "Retry the same command; if this repeats, report `post --version`.",
        )
    })?;
    let mut payload = ascii_escape_json(&payload);
    payload.push_str("\n---\n");
    payload.push_str(body);
    Ok(payload.into_bytes())
}

pub(crate) fn parse_channel_message(path: &Path) -> AppResult<ParsedChannelMessage> {
    let mut raw = String::new();
    File::open(path)
        .and_then(|mut file| file.read_to_string(&mut raw))
        .map_err(|error| AppError::io("read channel message file", path, error))?;
    let (head, body) = raw.split_once("\n---\n").ok_or_else(|| {
        AppError::config(
            path,
            "channel message has no '\\n---\\n' separator; restore a valid .msg file or move it aside",
        )
    })?;
    let message: ChannelMessage = serde_json::from_str(head)
        .map_err(|error| AppError::config(path, format!("malformed message JSON: {error}")))?;
    validate_channel_message(path, &message)?;
    if path.file_stem().and_then(|value| value.to_str()) != Some(message.id.as_str()) {
        return Err(AppError::config(
            path,
            format!(
                "channel message filename must be '{}.msg' to match its id",
                message.id
            ),
        ));
    }
    Ok(ParsedChannelMessage {
        message,
        body: body.to_owned(),
    })
}

pub(crate) fn validate_channel_message(path: &Path, message: &ChannelMessage) -> AppResult<()> {
    for (field, value) in [
        ("id", message.id.as_str()),
        ("from", message.from.as_str()),
        ("channel", message.channel.as_str()),
        ("sent", message.sent.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(AppError::config(
                path,
                format!("channel message field '{field}' is empty"),
            ));
        }
    }
    // The channel field is rendered UNQUOTED in watch text lines ("#name"),
    // so a hand-written .msg with a control character here could forge event
    // lines. The CLI mints only validated names; enforce the same at parse.
    if let Err(reason) = crate::mailbox::validate_component(&message.channel) {
        return Err(AppError::config(
            path,
            format!(
                "channel message field 'channel' ('{}') is not a path-safe name: {reason}",
                message.channel.escape_debug()
            ),
        ));
    }
    if message.channel.chars().any(char::is_control) {
        return Err(AppError::config(
            path,
            "channel message field 'channel' contains control characters",
        ));
    }
    // YYYYmmdd-HHMMSS-UUUUUU-<6 hex>: microsecond resolution keeps
    // lexicographic order ~= arrival order, which the reader's high-water
    // cursor depends on for completeness.
    if !is_canonical_channel_message_id(&message.id) {
        return Err(AppError::config(
            path,
            format!(
                "channel message id '{}' must match YYYYmmdd-HHMMSS-UUUUUU-<6 hex>",
                message.id
            ),
        ));
    }
    if let Some(event) = &message.event {
        if event != JOIN_EVENT && event != PROFILE_EVENT {
            return Err(AppError::config(
                path,
                format!(
                    "channel message event '{event}' is unknown; only '{JOIN_EVENT}' and '{PROFILE_EVENT}' exist"
                ),
            ));
        }
    }
    // Stamped profile fields render unquoted in chat banners and watch
    // text lines; a control character smuggled into a hand-written .msg
    // could forge whole lines, so refuse them at parse like `channel`.
    for (field, value) in [
        ("display_name", &message.display_name),
        ("pfp", &message.pfp),
    ] {
        if let Some(value) = value {
            if value.chars().any(crate::mailbox::refused_profile_char) {
                return Err(AppError::config(
                    path,
                    format!(
                        "channel message field '{field}' contains control, bidi, or line-separator characters"
                    ),
                ));
            }
        }
    }
    // `re` is untrusted store data rendered into text output; it must be a
    // canonical message id so short_id and reply resolution never panic or
    // trust a mismatched filename.
    if let Some(re) = &message.re {
        if !is_canonical_channel_message_id(re) {
            return Err(AppError::config(
                path,
                format!(
                    "channel message field 're' ('{}') must be a canonical channel message id",
                    re.escape_debug()
                ),
            ));
        }
    }
    for mention in &message.mentions {
        if mention.chars().any(crate::mailbox::refused_profile_char)
            || validate_room_name(mention).is_err()
        {
            return Err(AppError::config(
                path,
                format!(
                    "channel message mentions entry '{}' is not a valid room name",
                    mention.escape_debug()
                ),
            ));
        }
    }
    Ok(())
}

/// Canonical channel message id: `YYYYmmdd-HHMMSS-UUUUUU-<6 hex>` (29 bytes, ASCII).
pub(crate) fn is_canonical_channel_message_id(id: &str) -> bool {
    let id = id.as_bytes();
    id.len() == 29
        && id[..8].iter().all(u8::is_ascii_digit)
        && id[8] == b'-'
        && id[9..15].iter().all(u8::is_ascii_digit)
        && id[15] == b'-'
        && id[16..22].iter().all(u8::is_ascii_digit)
        && id[22] == b'-'
        && id[23..].iter().all(u8::is_ascii_hexdigit)
}

/// Find a channel message by full id or unique-enough prefix, across channels.
///
/// `post read` answers a miss by saying the id is "not unread, not already read,
/// not in the archive" and pointing at `post inbox` — two statements that are
/// both true and both useless when the id came from the doorbell, which hands
/// out channel message ids. Channel messages are not mail and never will be in
/// any of those three places, so the miss path has to look where they actually
/// live before claiming the id does not exist.
///
/// A miss is already an error path, so a stat per channel is affordable; the
/// scan stops at the first channel holding a match.
pub(crate) fn find_channel_message(
    context: &Context,
    prefix: &str,
) -> Option<(String, String, usize)> {
    let root = context.root.join(CHANNELS_DIR);
    let mut names: Vec<String> = fs::read_dir(&root)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    for channel in names {
        let messages = root.join(&channel).join("messages");
        let Ok(entries) = fs::read_dir(&messages) else {
            continue;
        };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("msg") {
                    return None;
                }
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .map(str::to_owned)
            })
            .collect();
        // Ids sort chronologically, which is what makes the depth below correct.
        ids.sort();
        if let Some(position) = ids.iter().position(|id| id.starts_with(prefix)) {
            // `--history N` renders the last N messages, so the depth that
            // includes the target is its distance from the end, inclusive.
            // `--since <id>` would NOT do -- it renders messages AFTER the id and
            // so would show everything except the one that was asked about.
            let depth = ids.len() - position;
            return Some((channel, ids[position].clone(), depth));
        }
    }
    None
}

pub(crate) fn message_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| AppError::io("list channel messages directory", directory, error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read channel messages entry", directory, error))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("msg") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) struct ChannelSummary {
    pub info: ChannelInfo,
    pub members: MemberMap,
    pub messages: usize,
    /// Archive mark in force, or None for a live channel.
    pub archived: Option<crate::channel_archive::ArchiveMark>,
}

pub(crate) fn list_channels(context: &Context) -> AppResult<Vec<ChannelSummary>> {
    let dir = context.root.join(CHANNELS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io("list channels directory", &dir, error)),
    };
    let mut summaries = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read channels entry", &dir, error))?;
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let paths = match ChannelPaths::new(context, &name) {
            Ok(paths) => paths,
            // A stray non-channel directory (or one with an invalid name)
            // is skipped rather than failing the whole listing.
            Err(_) => continue,
        };
        if !paths.exists() {
            continue;
        }
        let messages = match message_files(&paths.messages) {
            Ok(files) => files.len(),
            Err(_) => 0,
        };
        summaries.push(ChannelSummary {
            info: paths.load_info()?,
            members: paths.load_members()?,
            messages,
            // Fail open: an unreadable archive.json lists the channel as live
            // (visible) instead of failing the listing and every watch that
            // enumerates channels. `post doctor` reports the bad file.
            archived: crate::channel_archive::effective_mark(&paths).unwrap_or(None),
        });
    }
    summaries.sort_by(|left, right| left.info.name.cmp(&right.info.name));
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    fn test_context(label: &str, rooms: &str, rules: &str) -> (PathBuf, Context) {
        let root = test_root(&format!("channel-{label}"));
        fs::write(root.join("rooms.json"), rooms).expect("write rooms config");
        fs::write(root.join("rules.json"), rules).expect("write rules config");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        crate::participant::bind_test_actor(&context, "alpha");
        (root, context)
    }

    fn rooms_json(root: &Path) -> String {
        // Two registered rooms whose trees live under the test root so
        // acting_room can be steered by cwd-independent means: tests call
        // the store primitives directly instead of relying on cwd.
        format!(
            r#"{{"alpha": "{0}/alpha", "beta": "{0}/beta"}}"#,
            root.display()
        )
    }

    #[test]
    fn microsecond_ids_sort_chronologically_and_validate() {
        let (root, _context) = test_context("ids", r#"{"alpha": "/tmp"}"#, r#"{"blocked":[]}"#);
        let (first_ts, _) = local_timestamp_micros().expect("timestamp");
        let first = new_mail_id(&first_ts, 0).expect("id");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let (second_ts, _) = local_timestamp_micros().expect("timestamp");
        let second = new_mail_id(&second_ts, 0).expect("id");
        assert!(first < second, "{first} must sort before {second}");
        let message = ChannelMessage {
            id: first.clone(),
            from: "alpha".to_owned(),
            channel: "tax".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:00:00 -0500".to_owned(),
            from_participant: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        validate_channel_message(Path::new("<test>"), &message).expect("valid id shape");
        trash_test_root(&root);
    }

    #[test]
    fn control_characters_in_channel_field_are_refused_at_parse() {
        // watch text lines print "#<channel>" unquoted; a newline smuggled
        // into a hand-written .msg envelope must die in the validator.
        let message = ChannelMessage {
            id: "20260722-013000-000001-abc123".to_owned(),
            from: "alpha".to_owned(),
            channel: "tax\nFORGED".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:30:00 -0500".to_owned(),
            from_participant: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        let error = validate_channel_message(Path::new("<test>"), &message)
            .expect_err("control characters in channel must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
    }

    #[test]
    fn second_resolution_mail_id_is_refused_for_channel_messages() {
        let message = ChannelMessage {
            id: "20260722-012000-abcdef".to_owned(),
            from: "alpha".to_owned(),
            channel: "tax".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:20:00 -0500".to_owned(),
            from_participant: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        let error = validate_channel_message(Path::new("<test>"), &message)
            .expect_err("second-resolution ids must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
    }

    #[test]
    fn join_records_event_then_membership_and_send_roundtrips() {
        let (root, context) = test_context("join", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        fs::create_dir_all(root.join("alpha")).expect("alpha tree");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");

        // Drive the store primitives directly (acting_room is cwd-derived
        // and tests must not depend on process cwd).
        let event_id = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "=== alpha joined ===",
                event: Some(JOIN_EVENT),
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("join event");
        let mut members = MemberMap::new();
        members.insert("alpha".to_owned(), "2026-07-22 01:00:00 -0500".to_owned());
        let bytes = serde_json::to_vec_pretty(&members).expect("members json");
        atomic_replace(&paths.members_json, &bytes).expect("members write");

        let message_id = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "hello",
                body: "first message",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");
        assert!(
            event_id < message_id,
            "event must sort before the later send"
        );

        let files = message_files(&paths.messages).expect("list");
        assert_eq!(files.len(), 2);
        let parsed = parse_channel_message(&files[1]).expect("parse");
        assert_eq!(parsed.message.from, "alpha");
        assert_eq!(parsed.message.channel, "tax");
        assert_eq!(parsed.message.event, None);
        assert_eq!(parsed.body, "first message");
        let event = parse_channel_message(&files[0]).expect("parse event");
        assert_eq!(event.message.event.as_deref(), Some(JOIN_EVENT));
        trash_test_root(&root);
    }

    #[test]
    fn send_stamps_profile_as_of_send_time_and_rename_does_not_retcon() {
        let (root, context) = test_context("stamp", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        let actor = context.sender().expect("test actor").participant.id;
        let key = crate::profile::participant_key(&actor);
        fs::write(
            root.join("profiles.json"),
            format!(r#"{{"{key}": {{"name": "Lantern", "pfp": "🏮"}}}}"#),
        )
        .expect("profiles");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");

        let first = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "hi",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");
        // Rename after the first send; the stored first message must keep
        // the old name (history renders as-sent).
        fs::write(
            root.join("profiles.json"),
            format!(r#"{{"{key}": {{"name": "Coldwell", "pfp": "🏮"}}}}"#),
        )
        .expect("rename");
        let second = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "yo",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");

        let parse = |id: &str| {
            parse_channel_message(&paths.messages.join(format!("{id}.msg"))).expect("parse")
        };
        assert_eq!(
            parse(&first).message.display_name.as_deref(),
            Some("Lantern")
        );
        assert_eq!(
            parse(&second).message.display_name.as_deref(),
            Some("Coldwell")
        );
        assert_eq!(parse(&second).message.pfp.as_deref(), Some("🏮"));
        trash_test_root(&root);
    }

    #[test]
    fn blocked_route_bars_shared_membership_in_both_directions() {
        let (root, context) = test_context("blocked", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        fs::write(
            root.join("rules.json"),
            r#"{"blocked":[{"from":"*","to":"beta","reason":"armed instrument"}]}"#,
        )
        .expect("rules");
        let rooms = context.load_rooms().expect("load rooms");
        let rules = context.load_rules(&rooms).expect("load rules");

        // beta is already a member; alpha joining must be refused because
        // alpha -> beta is blocked, even though beta -> alpha is not.
        let members: MemberMap = [("beta".to_owned(), "t".to_owned())].into_iter().collect();
        let refused = members.keys().any(|member| {
            rules.blocked.iter().any(|rule| {
                rule.matches_route("alpha", member) || rule.matches_route(member, "alpha")
            })
        });
        assert!(refused, "pairwise check must catch the alpha->beta block");
        trash_test_root(&root);
    }

    #[test]
    fn list_channels_skips_strays_and_counts_messages() {
        let (root, context) = test_context("list", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");
        let info = ChannelInfo {
            name: "tax".to_owned(),
            created: "2026-07-22 01:00:00 -0500".to_owned(),
            created_by: "alpha".to_owned(),
            description: None,
        };
        let bytes = serde_json::to_vec_pretty(&info).expect("info json");
        atomic_replace(&paths.channel_json, &bytes).expect("info write");
        write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "hi",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");
        // A stray directory without channel.json must not break the listing.
        fs::create_dir_all(root.join(CHANNELS_DIR).join("not-a-channel")).expect("stray");

        let summaries = list_channels(&context).expect("list");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].info.name, "tax");
        assert_eq!(summaries[0].messages, 1);
        trash_test_root(&root);
    }

    #[test]
    fn embedded_at_in_registered_name_does_not_double_stamp() {
        use crate::model::RoomMap;
        use std::collections::BTreeMap;
        let mut rooms: RoomMap = BTreeMap::new();
        rooms.insert("foo.@bar".to_owned(), "/tmp".into());
        rooms.insert("bar".to_owned(), "/tmp".into());
        // One token, one mention: the embedded `@` inside the matched longer
        // name must not re-trigger and also stamp `bar`.
        assert_eq!(
            extract_mentions("@foo.@bar one token", &rooms),
            vec!["foo.@bar".to_owned()]
        );
        // A standalone `@bar` still stamps normally.
        assert_eq!(
            extract_mentions("plain @bar here", &rooms),
            vec!["bar".to_owned()]
        );
    }

    #[test]
    fn extract_mentions_takes_longest_registered_name_per_at() {
        use crate::model::RoomMap;
        use std::collections::BTreeMap;
        let mut rooms: RoomMap = BTreeMap::new();
        for name in [
            "foo",
            "foo.bar",
            "baz",
            "baz+qux",
            "café",
            "café.x",
            "claude",
            "claude-space",
        ] {
            rooms.insert(name.to_owned(), "/tmp".into());
        }
        assert_eq!(
            extract_mentions("ping @foo.bar please", &rooms),
            vec!["foo.bar".to_owned()]
        );
        assert_eq!(
            extract_mentions("see @baz+qux", &rooms),
            vec!["baz+qux".to_owned()]
        );
        assert_eq!(
            extract_mentions("hi @café.x", &rooms),
            vec!["café.x".to_owned()]
        );
        // Existing hyphen case: longer name wins; shorter must not also stamp.
        assert_eq!(
            extract_mentions("hey @claude-space", &rooms),
            vec!["claude-space".to_owned()]
        );
        assert_eq!(
            extract_mentions("hey @claude please", &rooms),
            vec!["claude".to_owned()]
        );
        // Non-ASCII letters are word characters: `@fooé` is not `@foo`, and
        // a leading `é` does not open a mention either.
        assert!(
            extract_mentions("ping @fooé please", &rooms).is_empty(),
            "@fooé must not stamp room foo"
        );
        assert!(
            extract_mentions("é@foo trailing", &rooms).is_empty(),
            "é@foo must not stamp: leading letter is a word char"
        );
        assert_eq!(
            extract_mentions("hi @café thanks", &rooms),
            vec!["café".to_owned()],
            "registered Unicode room names still stamp"
        );
    }

    #[test]
    fn malformed_re_is_refused_at_parse() {
        let message = ChannelMessage {
            id: "20260722-013000-000001-abc123".to_owned(),
            from: "alpha".to_owned(),
            channel: "tax".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:30:00 -0500".to_owned(),
            from_participant: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: Some("aaaaaaaéx".to_owned()),
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        let error = validate_channel_message(Path::new("<test>"), &message)
            .expect_err("non-canonical re must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
    }

    #[test]
    fn participant_review_unit_channel_send_stamps_actor_fields() {
        let (root, context) = test_context("participant-fields", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");
        let id = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "body",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("channel send");
        let raw = fs::read_to_string(paths.messages.join(format!("{id}.msg"))).expect("message");
        let head = raw.split_once("\n---\n").expect("separator").0;
        let message: serde_json::Value = serde_json::from_str(head).expect("message JSON");
        assert!(message["from_participant"].is_string());
        assert_eq!(message["address_kind"], "channel");
        trash_test_root(&root);
    }
}
