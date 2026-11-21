use crate::proto::{CodecError, ErrorCode};
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connecting to source socket {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("protocol: {0}")]
    Codec(#[from] CodecError),

    /// The source answered, and the answer was a refusal.
    #[error("source refused ({code:?}): {message}")]
    Source { code: ErrorCode, message: String },

    #[error("source closed the connection before answering")]
    NoResponse,

    #[error("expected a control message, got a frame")]
    UnexpectedFrame,

    #[error("expected a frame, got a control message")]
    UnexpectedControlMessage,

    #[error("source answered ok but omitted the `{0}` field")]
    MissingField(&'static str),
}

impl ClientError {
    /// True if the source refused because it is disabled — worth distinguishing from a
    /// transport failure, since the fix is "re-enable it", not "retry".
    pub fn is_disabled(&self) -> bool {
        matches!(
            self,
            ClientError::Source {
                code: ErrorCode::Disabled,
                ..
            }
        )
    }
}
