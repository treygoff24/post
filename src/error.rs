use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

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
    IoError,
}

impl ErrorCode {
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
            Self::IoError => "io_error",
        }
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub details: BTreeMap<String, Value>,
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
        let exit_code = match code {
            ErrorCode::InvalidArgument => 2,
            ErrorCode::UnknownRoom
            | ErrorCode::ReservedSender
            | ErrorCode::EmptyBody
            | ErrorCode::AmbiguousId => 65,
            ErrorCode::NotFound => 66,
            ErrorCode::BlockedRoute => 77,
            ErrorCode::ConfigInvalid => 78,
            ErrorCode::IoError => 75,
        };
        Self {
            code,
            message: message.into(),
            details: BTreeMap::new(),
            retryable: code == ErrorCode::IoError,
            suggested_fix: suggested_fix.into(),
            exit_code,
        }
    }

    pub fn detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
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
        .detail("path", path.display().to_string())
        .detail("reason", reason)
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
        .detail("operation", operation)
        .detail("path", path.display().to_string())
        .detail("reason", reason)
    }

    pub fn delivered_output_failure(source: impl std::fmt::Display) -> Self {
        let reason = source.to_string();
        let mut error = Self::new(
            ErrorCode::IoError,
            format!("mail was delivered but its receipt could not be written to stdout: {reason}"),
            "Do not resend this mail; inspect the recipient inbox or archive for the delivered message.",
        )
        .detail("operation", "write stdout after delivery")
        .detail("reason", reason);
        error.retryable = false;
        error.exit_code = 70;
        error
    }
}
