use thiserror::Error;
use ytm_protocol::{QueryError, QueryErrorCode};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("invalid storage payload: {0}")]
    InvalidPayload(String),
    #[error("stale account context: {0}")]
    StaleContext(String),
    #[error("account scope mismatch: {0}")]
    AccountMismatch(String),
    #[error("storage network timeout: {0}")]
    Timeout(String),
    #[error("storage network error: {0}")]
    Network(String),
}

impl StorageError {
    pub fn into_query_error(self) -> QueryError {
        match self {
            Self::Storage(msg) => QueryError {
                code: QueryErrorCode::StorageError,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg,
                request_id: None,
            },
            Self::InvalidPayload(msg) => QueryError {
                code: QueryErrorCode::InvalidPayload,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg,
                request_id: None,
            },
            Self::StaleContext(msg) => QueryError {
                code: QueryErrorCode::StaleContext,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg,
                request_id: None,
            },
            Self::AccountMismatch(msg) => QueryError {
                code: QueryErrorCode::AccountMismatch,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg,
                request_id: None,
            },
            Self::Timeout(msg) => QueryError {
                code: QueryErrorCode::Timeout,
                retriable: true,
                retry_after_ms: Some(1_000),
                safe_message: msg,
                request_id: None,
            },
            Self::Network(msg) => QueryError {
                code: QueryErrorCode::NetworkOffline,
                retriable: true,
                retry_after_ms: Some(1_000),
                safe_message: msg,
                request_id: None,
            },
        }
    }
}

impl From<StorageError> for QueryError {
    fn from(value: StorageError) -> Self {
        value.into_query_error()
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Storage(format!("sqlite operation failed: {err}"))
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(err: serde_json::Error) -> Self {
        Self::Storage(format!("json serialization failed: {err}"))
    }
}

impl From<std::io::Error> for StorageError {
    fn from(err: std::io::Error) -> Self {
        Self::Storage(format!("filesystem operation failed: {err}"))
    }
}
