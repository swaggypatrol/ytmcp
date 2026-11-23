use ytm_protocol::{QueryError, QueryErrorCode};
use ytmapi_rs::error::ErrorKind;

pub fn make_query_error(
    code: QueryErrorCode,
    retriable: bool,
    retry_after_ms: Option<u64>,
    safe_message: impl Into<String>,
) -> QueryError {
    QueryError {
        code,
        retriable,
        retry_after_ms,
        safe_message: sanitize_safe_message(&safe_message.into()),
        request_id: None,
    }
}

/// Strips sensitive tokens, headers, cookies, and oversized payloads from error strings.
pub fn sanitize_safe_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    let contains_sensitive = lower.contains("sapisid")
        || lower.contains("cookie:")
        || lower.contains("authorization:")
        || lower.contains("sapisidhash")
        || lower.contains("login_info=")
        || lower.contains("sid=")
        || lower.contains("hsid=");
    if contains_sensitive {
        return "sanitized catalog error (sensitive details redacted)".to_string();
    }
    const MAX_LEN: usize = 240;
    if trimmed.chars().count() > MAX_LEN {
        let mut out: String = trimmed.chars().take(MAX_LEN).collect();
        out.push_str("...");
        out
    } else {
        trimmed.to_string()
    }
}

/// Maps a `ytmapi-rs` error to a normalized `QueryError` without leaking raw response bodies or credentials.
pub fn classify_ytmapi_error(err: ytmapi_rs::Error) -> QueryError {
    classify_ytmapi_error_kind(err.into_kind())
}

pub(crate) fn classify_ytmapi_error_kind(kind: ErrorKind) -> QueryError {
    match kind {
        ErrorKind::Header => make_query_error(
            QueryErrorCode::AuthRequired,
            false,
            None,
            "missing or malformed YouTube Music session credentials",
        ),
        ErrorKind::OAuthTokenExpired { .. } => make_query_error(
            QueryErrorCode::AuthExpired,
            false,
            None,
            "YouTube Music session token has expired",
        ),
        ErrorKind::InvalidUserAgent(_) => make_query_error(
            QueryErrorCode::UnsupportedCapability,
            false,
            None,
            "YouTube Music rejected the client user agent",
        ),
        ErrorKind::OtherErrorCodeInResponse { code, message } => {
            classify_upstream_status(code, &message)
        }
        ErrorKind::Web { message } => classify_web_error(&message),
        ErrorKind::ApiStatusFailed => make_query_error(
            QueryErrorCode::UnavailableTrack,
            false,
            None,
            "YouTube Music returned STATUS_FAILED for the requested operation",
        ),
        ErrorKind::JsonParsing(_) => make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "upstream YouTube Music JSON shape could not be parsed",
        ),
        ErrorKind::InvalidResponse { response } => {
            let lower = response.to_ascii_lowercase();
            if lower.contains("429") || lower.contains("too many requests") {
                make_query_error(
                    QueryErrorCode::RateLimited,
                    true,
                    Some(5_000),
                    "YouTube Music rate limit reached",
                )
            } else if lower.contains("login_required") {
                // InnerTube's explicit reason; free text such as "sign in" in an HTML body is
                // not evidence that the session expired.
                make_query_error(
                    QueryErrorCode::AuthExpired,
                    false,
                    None,
                    "YouTube Music session expired or requires re-authentication",
                )
            } else {
                make_query_error(
                    QueryErrorCode::InvalidPayload,
                    false,
                    None,
                    "upstream YouTube Music returned a non-JSON response",
                )
            }
        }
        // A page that did not carry ytcfg (consent interstitial, partial load, shape change) says
        // nothing about the credentials; it is retried, not treated as an expired session.
        ErrorKind::UnableToParseYtCfg { .. } | ErrorKind::NoVisitorData => make_query_error(
            QueryErrorCode::InvalidPayload,
            true,
            Some(5_000),
            "unable to extract YouTube Music page configuration for session",
        ),
        ErrorKind::UnableToSerializeGoogleOAuthToken { .. } => make_query_error(
            QueryErrorCode::AuthExpired,
            false,
            None,
            "unable to serialize authentication token",
        ),
        ErrorKind::Io(_) => make_query_error(
            QueryErrorCode::NetworkOffline,
            true,
            Some(1_000),
            "network or I/O error while contacting YouTube Music",
        ),
        ErrorKind::SystemTimeError { .. } => make_query_error(
            QueryErrorCode::StorageError,
            false,
            None,
            "system clock error while signing YouTube Music request",
        ),
        ErrorKind::InvalidUploadFilename { .. } | ErrorKind::MissingUploadUrl => make_query_error(
            QueryErrorCode::UnsupportedCapability,
            false,
            None,
            "upload operations are unsupported",
        ),
        _ => make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "unexpected YouTube Music catalog error",
        ),
    }
}

pub fn classify_upstream_status(code: u64, message: &str) -> QueryError {
    let lower = message.to_ascii_lowercase();
    match code {
        401 => make_query_error(
            QueryErrorCode::AuthExpired,
            false,
            None,
            "YouTube Music authentication expired (HTTP 401)",
        ),
        403 => {
            if lower.contains("rate") || lower.contains("quota") || lower.contains("limit") {
                make_query_error(
                    QueryErrorCode::RateLimited,
                    true,
                    Some(5_000),
                    "YouTube Music quota or rate limit exceeded (HTTP 403)",
                )
            } else {
                make_query_error(
                    QueryErrorCode::AuthExpired,
                    false,
                    None,
                    "YouTube Music rejected session credentials (HTTP 403)",
                )
            }
        }
        429 => make_query_error(
            QueryErrorCode::RateLimited,
            true,
            Some(5_000),
            "YouTube Music rate limited requests (HTTP 429)",
        ),
        408 | 504 => make_query_error(
            QueryErrorCode::Timeout,
            true,
            Some(1_500),
            format!("YouTube Music request timed out (HTTP {code})"),
        ),
        400 | 404 => {
            if lower.contains("unavailable")
                || lower.contains("not found")
                || lower.contains("video")
                || lower.contains("playable")
            {
                make_query_error(
                    QueryErrorCode::UnavailableTrack,
                    false,
                    None,
                    format!("requested track or playlist is unavailable (HTTP {code})"),
                )
            } else {
                make_query_error(
                    QueryErrorCode::InvalidPayload,
                    false,
                    None,
                    format!("YouTube Music rejected request parameters (HTTP {code})"),
                )
            }
        }
        500..=599 => make_query_error(
            QueryErrorCode::NetworkOffline,
            true,
            Some(2_000),
            format!("YouTube Music upstream service unavailable (HTTP {code})"),
        ),
        _ => make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            format!("YouTube Music returned upstream code {code}"),
        ),
    }
}

fn classify_web_error(message: &str) -> QueryError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        make_query_error(
            QueryErrorCode::Timeout,
            true,
            Some(1_500),
            "YouTube Music request timed out",
        )
    } else if lower.contains("429") || lower.contains("too many requests") {
        make_query_error(
            QueryErrorCode::RateLimited,
            true,
            Some(5_000),
            "YouTube Music rate limit reached",
        )
    } else if lower.contains("401") || lower.contains("unauthorized") {
        make_query_error(
            QueryErrorCode::AuthExpired,
            false,
            None,
            "YouTube Music session expired",
        )
    } else {
        make_query_error(
            QueryErrorCode::NetworkOffline,
            true,
            Some(1_000),
            "unable to reach YouTube Music over the network",
        )
    }
}
