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
}

impl ErrorCode {
    pub const ALL: [Self; 13] = [
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
        }
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidArgument => 2,
            Self::UnknownRoom
            | Self::ReservedSender
            | Self::EmptyBody
            | Self::AmbiguousId
            | Self::DuplicateWorkspace
            | Self::NotAMember => 65,
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
        self.details.exact_fix = Some(value.into());
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

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InvalidArgument,
            message,
            "Run `post --help` or `post schema` and retry with the documented syntax.",
        )
    }

    pub fn config(path: &std::path::Path, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self::new(
            ErrorCode::ConfigInvalid,
            format!("configuration '{}' is invalid: {reason}", path.display()),
            "Fix the named file by hand, then run `post doctor`.",
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
            format!("mail was delivered but its receipt could not be written to stdout: {reason}"),
            "Do not resend this mail; inspect the recipient inbox or archive for the delivered message.",
        )
        .operation("write stdout after delivery")
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
