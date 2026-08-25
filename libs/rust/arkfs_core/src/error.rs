use std::io;
use thiserror::Error;

/// Shared error type for ArkFS libraries.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ArkError {
    #[error("not found: {what}")]
    NotFound { what: String },

    #[error("integrity check failed: {detail}")]
    Integrity { detail: String },

    #[error("quorum not satisfied: got {got} acks, need {need}")]
    Quorum { got: u32, need: u32 },

    #[error("I/O error ({kind:?}): {message}")]
    Io {
        kind: io::ErrorKind,
        message: String,
    },

    #[error("invalid attribute: {detail}")]
    AttrInvalid { detail: String },

    #[error("invalid argument: {detail}")]
    InvalidArgument { detail: String },

    #[error("conflict: {detail}")]
    Conflict { detail: String },

    #[error("not implemented: {detail}")]
    NotImplemented { detail: String },
}

impl ArkError {
    pub fn not_found(what: impl Into<String>) -> Self {
        ArkError::NotFound { what: what.into() }
    }

    pub fn integrity(detail: impl Into<String>) -> Self {
        ArkError::Integrity {
            detail: detail.into(),
        }
    }

    pub fn invalid_argument(detail: impl Into<String>) -> Self {
        ArkError::InvalidArgument {
            detail: detail.into(),
        }
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        ArkError::Conflict {
            detail: detail.into(),
        }
    }

    pub fn attr_invalid(detail: impl Into<String>) -> Self {
        ArkError::AttrInvalid {
            detail: detail.into(),
        }
    }

    pub fn not_implemented(detail: impl Into<String>) -> Self {
        ArkError::NotImplemented {
            detail: detail.into(),
        }
    }
}

impl From<io::Error> for ArkError {
    fn from(e: io::Error) -> Self {
        ArkError::Io {
            kind: e.kind(),
            message: e.to_string(),
        }
    }
}
