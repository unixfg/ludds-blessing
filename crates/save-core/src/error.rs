use serde::{Deserialize, Serialize};
use std::fmt;

pub type Result<T> = std::result::Result<T, CoreError>;

/// Stable machine-readable failures exposed over the desktop IPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    UnsupportedVersion,
    UnsupportedCompression,
    AmbiguousStructure,
    StaleSave,
    GameRunning,
    ValidationFailed,
    RecoveryRequired,
    InvalidPath,
    InvalidXml,
    ResourceLimit,
    InvalidEdit,
    NotFound,
    PermissionDenied,
    Io,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "UNSUPPORTED_VERSION",
            Self::UnsupportedCompression => "UNSUPPORTED_COMPRESSION",
            Self::AmbiguousStructure => "AMBIGUOUS_STRUCTURE",
            Self::StaleSave => "STALE_SAVE",
            Self::GameRunning => "GAME_RUNNING",
            Self::ValidationFailed => "VALIDATION_FAILED",
            Self::RecoveryRequired => "RECOVERY_REQUIRED",
            Self::InvalidPath => "INVALID_PATH",
            Self::InvalidXml => "INVALID_XML",
            Self::ResourceLimit => "RESOURCE_LIMIT",
            Self::InvalidEdit => "INVALID_EDIT",
            Self::NotFound => "NOT_FOUND",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::Io => "IO",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
}

impl CoreError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn ambiguous(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::AmbiguousStructure, message)
    }

    pub(crate) fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationFailed, message)
    }

    pub(crate) fn invalid_edit(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidEdit, message)
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for CoreError {}

impl From<std::io::Error> for CoreError {
    fn from(value: std::io::Error) -> Self {
        let code = match value.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            _ => ErrorCode::Io,
        };
        Self::new(code, value.to_string())
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::new(ErrorCode::ValidationFailed, value.to_string())
    }
}
