use thiserror::Error;
use ytm_protocol::{QueryError, QueryErrorCode, RequestId};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("core engine has stopped")]
    EngineStopped,
    #[error("core engine busy: {0}")]
    Busy(String),
    #[error("invalid command payload: {0}")]
    InvalidPayload(String),
    #[error("stale context: {0}")]
    StaleContext(String),
    #[error("bridge unavailable: {0}")]
    BridgeUnavailable(String),
    #[error("authentication required: {0}")]
    AuthRequired(String),
    #[error("account mismatch: {0}")]
    AccountMismatch(String),
    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),
    #[error("operation timed out: {0}")]
    Timeout(String),
    #[error("downstream error: {0}")]
    Query(String),
}

impl CoreError {
    pub fn to_query_error(&self, request_id: Option<RequestId>) -> QueryError {
        match self {
            Self::EngineStopped => QueryError {
                code: QueryErrorCode::Busy,
                retriable: true,
                retry_after_ms: Some(100),
                safe_message: self.to_string(),
                request_id,
            },
            Self::Busy(msg) => QueryError {
                code: QueryErrorCode::Busy,
                retriable: true,
                retry_after_ms: Some(100),
                safe_message: msg.clone(),
                request_id,
            },
            Self::InvalidPayload(msg) => QueryError {
                code: QueryErrorCode::InvalidPayload,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg.clone(),
                request_id,
            },
            Self::StaleContext(msg) => QueryError {
                code: QueryErrorCode::StaleContext,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg.clone(),
                request_id,
            },
            Self::BridgeUnavailable(msg) => QueryError {
                code: QueryErrorCode::BridgeIncompatible,
                retriable: true,
                retry_after_ms: Some(500),
                safe_message: msg.clone(),
                request_id,
            },
            Self::AuthRequired(msg) => QueryError {
                code: QueryErrorCode::AuthRequired,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg.clone(),
                request_id,
            },
            Self::AccountMismatch(msg) => QueryError {
                code: QueryErrorCode::AccountMismatch,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg.clone(),
                request_id,
            },
            Self::UnsupportedCapability(msg) => QueryError {
                code: QueryErrorCode::UnsupportedCapability,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg.clone(),
                request_id,
            },
            Self::Timeout(msg) => QueryError {
                code: QueryErrorCode::Timeout,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg.clone(),
                request_id,
            },
            Self::Query(msg) => QueryError {
                code: QueryErrorCode::StorageError,
                retriable: false,
                retry_after_ms: None,
                safe_message: msg.clone(),
                request_id,
            },
        }
    }
}

impl From<QueryError> for CoreError {
    fn from(value: QueryError) -> Self {
        match value.code {
            QueryErrorCode::Busy => Self::Busy(value.safe_message),
            QueryErrorCode::InvalidPayload => Self::InvalidPayload(value.safe_message),
            QueryErrorCode::StaleContext => Self::StaleContext(value.safe_message),
            QueryErrorCode::BridgeIncompatible => Self::BridgeUnavailable(value.safe_message),
            QueryErrorCode::AuthRequired | QueryErrorCode::AuthExpired => {
                Self::AuthRequired(value.safe_message)
            }
            QueryErrorCode::AccountMismatch => Self::AccountMismatch(value.safe_message),
            QueryErrorCode::UnsupportedCapability => {
                Self::UnsupportedCapability(value.safe_message)
            }
            QueryErrorCode::Timeout => Self::Timeout(value.safe_message),
            _ => Self::Query(value.safe_message),
        }
    }
}

impl From<CoreError> for QueryError {
    fn from(value: CoreError) -> Self {
        value.to_query_error(None)
    }
}
