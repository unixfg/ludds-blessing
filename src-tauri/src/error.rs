use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum ErrorCode {
    UnsupportedVersion,
    UnsupportedCompression,
    AmbiguousStructure,
    StaleSave,
    GameRunning,
    ValidationFailed,
    RecoveryRequired,
    InvalidArgument,
    NotFound,
    PermissionDenied,
    ProtectedSave,
    ReviewConsumed,
    IoError,
    InternalError,
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
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::NotFound => "NOT_FOUND",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::ProtectedSave => "PROTECTED_SAVE",
            Self::ReviewConsumed => "REVIEW_CONSUMED",
            Self::IoError => "IO_ERROR",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize, TS, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub detail: Option<String>,
    pub disk_changed: Option<bool>,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            detail: None,
            disk_changed: None,
        }
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn disk_changed(mut self) -> Self {
        self.disk_changed = Some(true);
        self
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn not_found(noun: &str) -> Self {
        Self::new(ErrorCode::NotFound, format!("{noun} was not found"))
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, message)
    }
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            _ => ErrorCode::IoError,
        };
        Self::new(code, "A local filesystem operation failed")
            .with_detail(format!("Filesystem error category: {:?}", error.kind()))
    }
}

impl From<save_core::CoreError> for CommandError {
    fn from(error: save_core::CoreError) -> Self {
        let code = match error.code.as_str() {
            "UNSUPPORTED_VERSION" => ErrorCode::UnsupportedVersion,
            "UNSUPPORTED_COMPRESSION" => ErrorCode::UnsupportedCompression,
            "AMBIGUOUS_STRUCTURE" => ErrorCode::AmbiguousStructure,
            "STALE_SAVE" => ErrorCode::StaleSave,
            "GAME_RUNNING" => ErrorCode::GameRunning,
            "RECOVERY_REQUIRED" => ErrorCode::RecoveryRequired,
            "NOT_FOUND" => ErrorCode::NotFound,
            "PERMISSION_DENIED" => ErrorCode::PermissionDenied,
            "INVALID_PATH" | "INVALID_EDIT" => ErrorCode::InvalidArgument,
            "IO" => ErrorCode::IoError,
            _ => ErrorCode::ValidationFailed,
        };
        let mut result = Self::new(code, error.message);
        if code == ErrorCode::StaleSave {
            result = result.disk_changed();
        }
        if matches!(code, ErrorCode::StaleSave | ErrorCode::GameRunning) {
            result = result.retryable();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_error_code_is_screaming_snake_case() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::UnsupportedVersion).unwrap(),
            "\"UNSUPPORTED_VERSION\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::RecoveryRequired).unwrap(),
            "\"RECOVERY_REQUIRED\""
        );
    }
}
