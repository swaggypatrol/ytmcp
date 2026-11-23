use serde::{Deserialize, Serialize};
use ytm_protocol::{AccountScope, QueryError, QueryErrorCode};

use crate::probe::error::make_query_error;

const CURSOR_PREFIX: &str = "ytmg1:";
const CURSOR_VERSION: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CursorKind {
    SearchSongs,
    LibraryPlaylists,
    PlaylistTracks,
    LikedSongs,
    FreshPlaylist,
    SongRadio,
    Discovery,
    ExtendedMedia,
    Library,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorPayload {
    v: u8,
    kind: CursorKind,
    target: String,
    account_scope: String,
    locale: String,
    /// Upstream continuation; `None` means the first upstream page.
    token: Option<String>,
    /// Items of that upstream page already returned to the caller.
    skip: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    page_start: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_fingerprint: Option<String>,
}

/// Position inside an upstream page sequence: which upstream page, and how many of its items
/// were already returned. Lets a `limit` smaller than the upstream page size page correctly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorPosition {
    pub token: Option<String>,
    pub skip: usize,
    pub(super) page_start: usize,
    pub(super) incomplete: bool,
    pub(super) window_fingerprint: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

pub fn encode_cursor(
    kind: CursorKind,
    target: &str,
    account_scope: &AccountScope,
    locale: &str,
    position: &CursorPosition,
) -> Result<String, QueryError> {
    if position.token.as_deref().is_some_and(str::is_empty)
        || (position.token.is_none() && position.skip == 0)
    {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "cannot encode an empty pagination position",
        ));
    }
    let payload = CursorPayload {
        v: CURSOR_VERSION,
        kind,
        target: normalize_target(kind, target),
        account_scope: account_scope.as_str().to_string(),
        locale: locale.to_string(),
        token: position.token.clone(),
        skip: position.skip,
        page_start: position.page_start,
        incomplete: position.incomplete,
        window_fingerprint: position.window_fingerprint.clone(),
    };
    let json_bytes = serde_json::to_vec(&payload).map_err(|_| {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "failed to serialize pagination cursor",
        )
    })?;
    Ok(format!("{CURSOR_PREFIX}{}", hex_encode(&json_bytes)))
}

pub fn decode_cursor(
    raw_cursor: &str,
    expected_kind: CursorKind,
    expected_target: &str,
    expected_account_scope: &AccountScope,
    expected_locale: &str,
) -> Result<CursorPosition, QueryError> {
    if raw_cursor.len() > 16 * 1024 {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "pagination cursor exceeds the allowed size",
        ));
    }
    let encoded = raw_cursor.strip_prefix(CURSOR_PREFIX).ok_or_else(|| {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "invalid pagination cursor format",
        )
    })?;
    let bytes = hex_decode(encoded).ok_or_else(|| {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "corrupted pagination cursor",
        )
    })?;
    let payload: CursorPayload = serde_json::from_slice(&bytes).map_err(|_| {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "invalid pagination cursor payload",
        )
    })?;
    if payload.v != CURSOR_VERSION || payload.token.as_deref().is_some_and(str::is_empty) {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "unsupported pagination cursor version",
        ));
    }
    if payload.account_scope != expected_account_scope.as_str() {
        return Err(make_query_error(
            QueryErrorCode::StaleContext,
            false,
            None,
            "pagination cursor belongs to a different account scope",
        ));
    }
    if payload.locale != expected_locale {
        return Err(make_query_error(
            QueryErrorCode::StaleContext,
            false,
            None,
            "pagination cursor does not match active locale",
        ));
    }
    if payload.kind != expected_kind
        || payload.target != normalize_target(expected_kind, expected_target)
    {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "pagination cursor cannot be reused across different queries",
        ));
    }
    Ok(CursorPosition {
        token: payload.token,
        skip: payload.skip,
        page_start: payload.page_start,
        incomplete: payload.incomplete,
        window_fingerprint: payload.window_fingerprint,
    })
}

fn normalize_target(kind: CursorKind, target: &str) -> String {
    match kind {
        CursorKind::SearchSongs => target.trim().to_ascii_lowercase(),
        CursorKind::PlaylistTracks
        | CursorKind::FreshPlaylist
        | CursorKind::SongRadio
        | CursorKind::Discovery
        | CursorKind::ExtendedMedia
        | CursorKind::Library => target.trim().to_string(),
        CursorKind::LibraryPlaylists | CursorKind::LikedSongs => String::new(),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(LUT[(b >> 4) as usize] as char);
        out.push(LUT[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(input: &str) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(2) {
        return None;
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for &[hi_b, lo_b] in bytes.as_chunks::<2>().0 {
        let hi = hex_nibble(hi_b)?;
        let lo = hex_nibble(lo_b)?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
