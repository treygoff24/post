use crate::model::BlockingRule;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub(crate) type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_fix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<BlockingRule>,
    /// Unread channel messages that blocked a crossed send (last 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missed: Option<Vec<MissedChannelMessage>>,
}

/// One unread channel message surfaced in a `crossed_send` bounce so the
/// sender can revise before `--anyway`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissedChannelMessage {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub sent: String,
    pub body: String,
    /// Present only on signed-looking messages from the owner room (A0a
    /// Decision 3): whether the sidecar signature cryptographically verifies
    /// and the channel text matches the signed payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_verified: Option<bool>,
    /// Identity-layer fields, carried raw: crossed sends are exactly the
    /// concurrent-instance moment where attribution matters (Sol's M1
    /// review). Absent keeps the old bounce shape byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_provenance: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnknownRoom,
    BlockedRoute,
    ReservedSender,
    EmptyBody,
    AmbiguousId,
    NotFound,
    InvalidArgument,
    ConfigInvalid,
    DuplicateWorkspace,
    IoError,
    DeliveredOutputFailure,
    DeliveredUnarchived,
    NotAMember,
    NoParticipant,
    NotYet,
    /// Unseen messages from other rooms exist in the channel; send was not delivered.
    CrossedSend,
    /// A read's stdin is an open pipe that stayed silent through the bounded
    /// readiness wait: post cannot tell a read from a body still on its way.
    InputAmbiguous,
}

impl ErrorCode {
    pub const ALL: [Self; 17] = [
        Self::UnknownRoom,
        Self::BlockedRoute,
        Self::ReservedSender,
        Self::EmptyBody,
        Self::AmbiguousId,
        Self::NotFound,
        Self::InvalidArgument,
        Self::ConfigInvalid,
        Self::DuplicateWorkspace,
        Self::IoError,
        Self::DeliveredOutputFailure,
        Self::DeliveredUnarchived,
        Self::NotAMember,
        Self::NoParticipant,
        Self::NotYet,
        Self::CrossedSend,
        Self::InputAmbiguous,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownRoom => "unknown_room",
            Self::BlockedRoute => "blocked_route",
            Self::ReservedSender => "reserved_sender",
            Self::EmptyBody => "empty_body",
            Self::AmbiguousId => "ambiguous_id",
            Self::NotFound => "not_found",
            Self::InvalidArgument => "invalid_argument",
            Self::ConfigInvalid => "config_invalid",
            Self::DuplicateWorkspace => "duplicate_workspace",
            Self::IoError => "io_error",
            Self::DeliveredOutputFailure => "delivered_output_failure",
            Self::DeliveredUnarchived => "delivered_unarchived",
            Self::NotAMember => "not_a_member",
            Self::NoParticipant => "no_participant",
            Self::NotYet => "not_yet",
            Self::CrossedSend => "crossed_send",
            Self::InputAmbiguous => "input_ambiguous",
        }
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidArgument | Self::InputAmbiguous => 2,
            Self::UnknownRoom
            | Self::ReservedSender
            | Self::EmptyBody
            | Self::AmbiguousId
            | Self::DuplicateWorkspace
            | Self::NotAMember
            | Self::NoParticipant
            | Self::CrossedSend => 65,
            Self::NotYet => 69,
            Self::NotFound => 66,
            Self::BlockedRoute => 77,
            Self::ConfigInvalid => 78,
            Self::IoError => 75,
            Self::DeliveredOutputFailure | Self::DeliveredUnarchived => 70,
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(self, Self::IoError)
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Box<ErrorDetails>,
    pub retryable: bool,
    pub suggested_fix: String,
    pub exit_code: i32,
}

impl AppError {
    pub fn new(
        code: ErrorCode,
        message: impl Into<String>,
        suggested_fix: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details: Box::default(),
            retryable: code.retryable(),
            suggested_fix: suggested_fix.into(),
            exit_code: code.exit_code(),
        }
    }

    pub fn archive_path(mut self, value: impl Into<String>) -> Self {
        self.details.archive_path = Some(value.into());
        self
    }

    pub fn did_you_mean(mut self, value: impl Into<String>) -> Self {
        self.details.did_you_mean = Some(value.into());
        self
    }

    pub fn exact_fix(mut self, value: impl Into<String>) -> Self {
        let value = value.into();
        // README: exact_fix "holds a command that runs as written". A
        // placeholder makes that false, and the caller most likely to paste it
        // unedited is an agent. Enforced here rather than per-test because
        // runnability was opt-in (`run_fix`) and crossed_send's fix shipped a
        // `'<revised text>'` placeholder for weeks under a green suite.
        debug_assert!(
            !contains_placeholder(&value),
            "exact_fix must run as written, but contains a placeholder: {value}"
        );
        self.details.exact_fix = Some(value);
        self
    }

    pub fn id(mut self, value: impl Into<String>) -> Self {
        self.details.id = Some(value.into());
        self
    }

    pub fn inbox_path(mut self, value: impl Into<String>) -> Self {
        self.details.inbox_path = Some(value.into());
        self
    }

    pub fn input(mut self, value: impl Into<String>) -> Self {
        self.details.input = Some(value.into());
        self
    }

    pub fn matches(mut self, value: Vec<String>) -> Self {
        self.details.matches = Some(value);
        self
    }

    pub fn operation(mut self, value: impl Into<String>) -> Self {
        self.details.operation = Some(value.into());
        self
    }

    pub fn path(mut self, value: impl Into<String>) -> Self {
        self.details.path = Some(value.into());
        self
    }

    pub fn reason(mut self, value: impl Into<String>) -> Self {
        self.details.reason = Some(value.into());
        self
    }

    pub fn registered_path(mut self, value: impl Into<String>) -> Self {
        self.details.registered_path = Some(value.into());
        self
    }

    pub fn room(mut self, value: impl Into<String>) -> Self {
        self.details.room = Some(value.into());
        self
    }

    pub fn rule(mut self, value: BlockingRule) -> Self {
        self.details.rule = Some(value);
        self
    }

    pub fn missed(mut self, value: Vec<MissedChannelMessage>) -> Self {
        self.details.missed = Some(value);
        self
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InvalidArgument,
            message,
            "Run `post --help` or `post schema` and retry with the documented syntax.",
        )
    }

    pub fn no_participant(bind_key_available: bool) -> Self {
        let error = if bind_key_available {
            Self::new(
                ErrorCode::NoParticipant,
                "participant: unbound (run: post participant bind)",
                "Bind this conversation first (run: post participant bind).",
            )
            .exact_fix("post participant bind")
        } else {
            Self::new(
                ErrorCode::NoParticipant,
                "participant: unbound (run: post participant bind --new, then run the printed export POST_PARTICIPANT=... command)",
                "Create a participant with `post participant bind --new`, then run the printed `export POST_PARTICIPANT=...` command before retrying.",
            )
        };
        error.reason("no bound participant record")
    }

    pub fn not_yet(task: &str) -> Self {
        Self::new(
            ErrorCode::NotYet,
            format!("this command surface is declared but its body belongs to {task}"),
            format!("Complete and integrate task {task}, then retry the same command."),
        )
        .reason(format!("implementation deferred to {task}"))
    }

    pub fn config(path: &std::path::Path, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self::new(
            ErrorCode::ConfigInvalid,
            format!("configuration '{}' is invalid: {reason}", path.display()),
            "Run `post doctor --fix` to recreate missing config, or fix the named file by hand, then run `post doctor`.",
        )
        .path(path.display().to_string())
        .reason(reason)
    }

    pub fn io(operation: &str, path: &std::path::Path, source: impl std::fmt::Display) -> Self {
        let reason = source.to_string();
        Self::new(
            ErrorCode::IoError,
            format!("failed to {operation} '{}': {reason}", path.display()),
            format!(
                "Check that '{}' exists and is readable/writable, then retry the same command.",
                path.display()
            ),
        )
        .operation(operation)
        .path(path.display().to_string())
        .reason(reason)
    }

    pub fn delivered_output_failure(source: impl std::fmt::Display) -> Self {
        let reason = source.to_string();
        Self::new(
            ErrorCode::DeliveredOutputFailure,
            format!("operation was committed but its receipt could not be written to stdout: {reason}"),
            "Do not retry blindly; inspect the recipient inbox, archive, or channel state for the committed operation.",
        )
        .operation("write stdout after committed operation")
        .reason(reason)
    }

    pub fn delivered_unarchived(
        id: &str,
        inbox: &std::path::Path,
        archive: &std::path::Path,
        source: impl std::fmt::Display,
    ) -> Self {
        let reason = source.to_string();
        Self::new(
            ErrorCode::DeliveredUnarchived,
            format!(
                "mail '{id}' was delivered to '{}' but could not be archived at '{}': {reason}",
                inbox.display(),
                archive.display()
            ),
            "Do not resend this mail; run `post doctor` and reconcile the delivered and archive copies by hand.",
        )
        .id(id)
        .inbox_path(inbox.display().to_string())
        .archive_path(archive.display().to_string())
        .reason(reason)
    }
}

/// A `<...>` placeholder: a shell word whose ENTIRE content is one bracket
/// pair, quoted or not -- `<ROOM>`, `--body '<revised text>'`.
///
/// The whole-word rule is what makes this safe to enforce. exact_fix now
/// reproduces the caller's real message body, so brackets appear in legitimate
/// commands all the time: prose mentioning a `<tag>`, pasted XML, a redirect.
/// Those are brackets INSIDE an argument; a placeholder IS the argument. A
/// guard that fired on `--body 'see the <tag> here'` would be switched off by
/// the first person it lied to, and then it protects nothing.
fn contains_placeholder(value: &str) -> bool {
    shell_words(value).iter().any(|word| {
        let bare: String = word.chars().filter(|&c| c != '\'').collect();
        let Some(inner) = bare.strip_prefix('<').and_then(|r| r.strip_suffix('>')) else {
            return false;
        };
        // `<note>hi</note>` is a body, not a placeholder: a placeholder holds
        // one unbroken run of text between exactly one pair of brackets.
        !inner.contains('<') && !inner.contains('>')
    })
}

/// Split on whitespace, keeping single-quoted runs together so
/// `--body '<revised text>'` stays one word. Not a full shell parser: it only
/// needs to agree with `shell_quote`, which is what builds every fix.
fn shell_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in value.chars() {
        match c {
            '\'' => {
                quoted = !quoted;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod exact_fix_contract {
    use super::contains_placeholder;

    #[test]
    fn placeholders_are_caught_and_shell_redirection_is_not() {
        // The shapes that actually shipped in this binary.
        assert!(contains_placeholder(
            "post chat 'x' --send --anyway --body '<revised text>'"
        ));
        assert!(contains_placeholder("post send --to <ROOM> --body <TEXT>"));
        assert!(contains_placeholder("post chat <CHANNEL>"));

        // Must NOT fire on real commands, or the guard gets disabled by whoever
        // hits the first false positive. `<` with a space after it is
        // redirection, and a lone `>` is not a placeholder either.
        assert!(!contains_placeholder("post chat 'ops' --send --anyway"));
        assert!(!contains_placeholder("post chat 'ops' --send < body.txt"));
        assert!(!contains_placeholder("post send --to 'a' --body 'x > y'"));
        assert!(!contains_placeholder(
            "post send --to 'a' --body 'a<b and c'"
        ));

        // Reported by Fable at the 078f6bd gate. exact_fix now reproduces the
        // caller's real body, so legitimate angle brackets reach this check.
        // Brackets inside an argument are data; only a whole argument that is
        // nothing but a bracket pair is a placeholder.
        assert!(!contains_placeholder(
            "post send --to 'participant:test-self' --body 'see the <tag> here'"
        ));
        assert!(!contains_placeholder(
            "post chat 'ops' --send --anyway --body '<note>hi</note>'"
        ));
        // A template outside the quotes is still caught, body or no body.
        assert!(contains_placeholder(
            "post send --to <ROOM> --body 'a real body'"
        ));
    }
}
