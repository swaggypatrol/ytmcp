use crate::error::StorageError;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use ytm_protocol::{
    AccountEpoch, AccountScope, AlbumDetail, AlbumId, ArtistDetail, ArtistId, Freshness, HomeFeed,
    LyricsKind, LyricsMatchMetadata, LyricsResult, PagedResult, PlaybackRef, PlaylistDetail,
    PlaylistId, PlaylistSummary, SearchResult, Track, VideoId,
};

pub const DEFAULT_LIBRARY_LIST_TTL_SECS: u64 = 15 * 60;
pub const DEFAULT_DETAIL_TTL_SECS: u64 = 60 * 60;
pub const DEFAULT_LYRICS_HIT_TTL_SECS: u64 = 30 * 24 * 60 * 60;
pub const DEFAULT_LYRICS_NEGATIVE_TTL_SECS: u64 = 24 * 60 * 60;
/// Local play history kept per account; older entries are pruned on insert.
pub const MAX_RECENT_PLAYBACK_PER_ACCOUNT: i64 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCacheContext {
    pub account_scope: AccountScope,
    pub account_epoch: AccountEpoch,
    pub locale: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CachedValue<T> {
    pub value: T,
    pub account_scope: AccountScope,
    pub locale: String,
    pub provider_version: String,
    pub fetched_at_ms: u64,
    pub ttl_secs: u64,
    pub freshness: Freshness,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecentPlaybackRecord {
    pub track: Track,
    pub playback_ref: Option<PlaybackRef>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub recorded_at_ms: u64,
    pub source_context: Option<String>,
}

pub fn evaluate_freshness(fetched_at_ms: u64, ttl_secs: u64, now_ms: u64) -> Freshness {
    let ttl_ms = ttl_secs.saturating_mul(1_000);
    if now_ms < fetched_at_ms.saturating_add(ttl_ms) {
        Freshness::Fresh
    } else {
        Freshness::Stale
    }
}

pub fn compute_lyrics_cache_key(meta: &LyricsMatchMetadata, provider: &str) -> String {
    hex::encode(Sha256::digest(
        lyrics_metadata_key(meta, provider).as_bytes(),
    ))
}

pub(crate) fn lyrics_metadata_key(meta: &LyricsMatchMetadata, provider: &str) -> String {
    serde_json::json!({
        "provider": provider.trim().to_ascii_lowercase(),
        "videoId": meta.video_id.as_str(),
        "title": meta.title.trim().to_ascii_lowercase(),
        "artist": meta.artist.trim().to_ascii_lowercase(),
        "album": meta.album.as_deref().map(|s| s.trim().to_ascii_lowercase()),
        "durationMs": meta.duration_ms,
        "versionTag": meta.version_tag.as_deref().map(|s| s.trim().to_ascii_lowercase()),
    })
    .to_string()
}

pub fn register_account_epoch_tx(
    tx: &Transaction<'_>,
    account_scope: &AccountScope,
    account_epoch: &AccountEpoch,
    now_ms: u64,
) -> Result<(), StorageError> {
    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT account_scope, state FROM account_epochs WHERE account_epoch = ?1",
            params![account_epoch.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    if let Some((existing_scope, state)) = existing {
        if state != "active" {
            return Err(StorageError::StaleContext(
                "cannot reactivate an invalidated account epoch".to_string(),
            ));
        }
        if existing_scope != account_scope.as_str() {
            return Err(StorageError::AccountMismatch(
                "account epoch belongs to a different account scope".to_string(),
            ));
        }
        return Ok(());
    }

    // Supersede any previously active epoch for this scope.
    tx.execute(
        "UPDATE account_epochs SET state = 'invalidated', updated_at_ms = ?1 WHERE account_scope = ?2 AND state = 'active'",
        params![now_i64, account_scope.as_str()],
    )?;

    tx.execute(
        "INSERT INTO account_epochs (account_epoch, account_scope, state, updated_at_ms) VALUES (?1, ?2, 'active', ?3)",
        params![account_epoch.as_str(), account_scope.as_str(), now_i64],
    )?;
    Ok(())
}

pub fn invalidate_account_epoch_tx(
    tx: &Transaction<'_>,
    account_epoch: &AccountEpoch,
    now_ms: u64,
) -> Result<(), StorageError> {
    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    tx.execute(
        r#"
INSERT INTO account_epochs (account_epoch, account_scope, state, updated_at_ms)
VALUES (?1, '', 'invalidated', ?2)
ON CONFLICT(account_epoch) DO UPDATE SET
    state = 'invalidated',
    updated_at_ms = excluded.updated_at_ms
"#,
        params![account_epoch.as_str(), now_i64],
    )?;
    Ok(())
}

pub fn ensure_account_epoch_writable_tx(
    tx: &Transaction<'_>,
    account_scope: &AccountScope,
    account_epoch: &AccountEpoch,
    now_ms: u64,
) -> Result<(), StorageError> {
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT account_scope, state FROM account_epochs WHERE account_epoch = ?1",
            params![account_epoch.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    match existing {
        Some((scope, state)) => {
            if state != "active" {
                return Err(StorageError::StaleContext(
                    "late write rejected for invalidated account epoch".to_string(),
                ));
            }
            if scope != account_scope.as_str() {
                return Err(StorageError::AccountMismatch(
                    "late write rejected: account epoch does not match account scope".to_string(),
                ));
            }
            Ok(())
        }
        None => {
            let active_for_scope: Option<String> = tx
                .query_row(
                    "SELECT account_epoch FROM account_epochs WHERE account_scope = ?1 AND state = 'active' LIMIT 1",
                    params![account_scope.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            if active_for_scope.is_some() {
                return Err(StorageError::StaleContext(
                    "late write rejected: superseded by newer active account epoch".to_string(),
                ));
            }
            let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO account_epochs (account_epoch, account_scope, state, updated_at_ms) VALUES (?1, ?2, 'active', ?3)",
                params![account_epoch.as_str(), account_scope.as_str(), now_i64],
            )?;
            Ok(())
        }
    }
}

/// Retire a binding whether its registration transaction ran before or after logout.
pub(crate) fn clear_pending_account_tx(
    tx: &Transaction<'_>,
    epoch: &AccountEpoch,
    now_ms: u64,
) -> Result<(), StorageError> {
    let scope: Option<String> = tx.query_row(
        "SELECT account_scope FROM account_epochs WHERE account_epoch = ?1 AND state = 'active'",
        params![epoch.as_str()],
        |row| row.get(0),
    ).optional()?;
    if let Some(scope) = scope {
        let scope = AccountScope::new(scope)
            .map_err(|err| StorageError::Storage(format!("invalid stored scope: {err}")))?;
        clear_account_data_tx(tx, &scope, now_ms)?;
    } else {
        // A delayed registration will see the tombstone and cannot revive this epoch.
        invalidate_account_epoch_tx(tx, epoch, now_ms)?;
    }
    Ok(())
}

pub fn clear_account_data_tx(
    tx: &Transaction<'_>,
    account_scope: &AccountScope,
    now_ms: u64,
) -> Result<(), StorageError> {
    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let scope = account_scope.as_str();
    tx.execute(
        "UPDATE account_epochs SET state = 'invalidated', updated_at_ms = ?1 WHERE account_scope = ?2",
        params![now_i64, scope],
    )?;
    tx.execute(
        "DELETE FROM playlists WHERE account_scope = ?1",
        params![scope],
    )?;
    tx.execute(
        "DELETE FROM playlist_tracks WHERE account_scope = ?1",
        params![scope],
    )?;
    tx.execute(
        "DELETE FROM catalog_cache WHERE account_scope = ?1",
        params![scope],
    )?;
    tx.execute(
        "DELETE FROM recent_playback WHERE account_scope = ?1",
        params![scope],
    )?;
    Ok(())
}

pub fn put_catalog_entry_tx<T: Serialize>(
    tx: &Transaction<'_>,
    ctx: &AccountCacheContext,
    entry_kind: &str,
    cache_key: &str,
    payload: &T,
    ttl_secs: u64,
    now_ms: u64,
) -> Result<(), StorageError> {
    ensure_account_epoch_writable_tx(tx, &ctx.account_scope, &ctx.account_epoch, now_ms)?;
    let payload_json = serde_json::to_string(payload)?;
    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let ttl_i64 = i64::try_from(ttl_secs).unwrap_or(i64::MAX);

    tx.execute(
        r#"
INSERT INTO catalog_cache
    (account_scope, locale, entry_kind, cache_key, payload_json, provider_version, fetched_at_ms, ttl_secs)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
ON CONFLICT(account_scope, locale, entry_kind, cache_key) DO UPDATE SET
    payload_json = excluded.payload_json,
    provider_version = excluded.provider_version,
    fetched_at_ms = excluded.fetched_at_ms,
    ttl_secs = excluded.ttl_secs
"#,
        params![
            ctx.account_scope.as_str(),
            &ctx.locale,
            entry_kind,
            cache_key,
            payload_json,
            &ctx.provider_version,
            now_i64,
            ttl_i64
        ],
    )?;
    Ok(())
}

pub fn get_catalog_entry_tx<T: DeserializeOwned>(
    conn: &Connection,
    account_scope: &AccountScope,
    locale: &str,
    entry_kind: &str,
    cache_key: &str,
    now_ms: u64,
) -> Result<Option<CachedValue<T>>, StorageError> {
    let row: Option<(String, String, i64, i64)> = conn
        .query_row(
            r#"
SELECT payload_json, provider_version, fetched_at_ms, ttl_secs
FROM catalog_cache
WHERE account_scope = ?1 AND locale = ?2 AND entry_kind = ?3 AND cache_key = ?4
"#,
            params![account_scope.as_str(), locale, entry_kind, cache_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    let Some((payload_json, provider_version, fetched_at_ms_i64, ttl_secs_i64)) = row else {
        return Ok(None);
    };
    let fetched_at_ms = u64::try_from(fetched_at_ms_i64.max(0)).unwrap_or(0);
    let ttl_secs = u64::try_from(ttl_secs_i64.max(0)).unwrap_or(0);
    let value: T = serde_json::from_str(&payload_json)?;
    let freshness = evaluate_freshness(fetched_at_ms, ttl_secs, now_ms);

    Ok(Some(CachedValue {
        value,
        account_scope: account_scope.clone(),
        locale: locale.to_string(),
        provider_version,
        fetched_at_ms,
        ttl_secs,
        freshness,
    }))
}

pub fn put_library_playlists_tx(
    tx: &Transaction<'_>,
    ctx: &AccountCacheContext,
    cursor_key: &str,
    page: &PagedResult<PlaylistSummary>,
    ttl_secs: u64,
    now_ms: u64,
) -> Result<(), StorageError> {
    put_catalog_entry_tx(
        tx,
        ctx,
        LIBRARY_PLAYLIST_PAGE_KIND,
        cursor_key,
        page,
        ttl_secs,
        now_ms,
    )?;

    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let ttl_i64 = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
    for (idx, pl) in page.items.iter().enumerate() {
        let cover_json = pl.cover.as_ref().map(serde_json::to_string).transpose()?;
        tx.execute(
            r#"
INSERT INTO playlists
    (account_scope, locale, playlist_id, browse_id, title, subtitle, track_count, cover_json, position_idx, provider_version, fetched_at_ms, ttl_secs)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT(account_scope, locale, playlist_id) DO UPDATE SET
    browse_id = excluded.browse_id,
    title = excluded.title,
    subtitle = excluded.subtitle,
    track_count = excluded.track_count,
    cover_json = excluded.cover_json,
    position_idx = excluded.position_idx,
    provider_version = excluded.provider_version,
    fetched_at_ms = excluded.fetched_at_ms,
    ttl_secs = excluded.ttl_secs
"#,
            params![
                ctx.account_scope.as_str(),
                &ctx.locale,
                pl.playlist_id.as_str(),
                pl.browse_id.as_ref().map(|b| b.as_str()),
                &pl.title,
                pl.subtitle.as_deref(),
                pl.track_count,
                cover_json,
                i64::try_from(idx).unwrap_or(i64::MAX),
                &ctx.provider_version,
                now_i64,
                ttl_i64
            ],
        )?;
    }

    Ok(())
}

pub fn put_playlist_detail_tx(
    tx: &Transaction<'_>,
    ctx: &AccountCacheContext,
    playlist_id: &PlaylistId,
    cursor_key: &str,
    detail: &PlaylistDetail,
    ttl_secs: u64,
    now_ms: u64,
) -> Result<(), StorageError> {
    let cache_key = playlist_detail_cache_key(playlist_id, cursor_key);
    put_catalog_entry_tx(
        tx,
        ctx,
        PLAYLIST_DETAIL_PAGE_KIND,
        &cache_key,
        detail,
        ttl_secs,
        now_ms,
    )?;

    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let ttl_i64 = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
    tx.execute(
        "DELETE FROM playlist_tracks WHERE account_scope = ?1 AND locale = ?2 AND playlist_id = ?3 AND cursor_key = ?4",
        params![
            ctx.account_scope.as_str(),
            &ctx.locale,
            playlist_id.as_str(),
            cursor_key
        ],
    )?;

    for (idx, track) in detail.tracks.items.iter().enumerate() {
        let track_json = serde_json::to_string(track)?;
        tx.execute(
            r#"
INSERT INTO playlist_tracks
    (account_scope, locale, playlist_id, cursor_key, position_idx, video_id, track_json, next_cursor, provider_version, fetched_at_ms, ttl_secs)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
"#,
            params![
                ctx.account_scope.as_str(),
                &ctx.locale,
                playlist_id.as_str(),
                cursor_key,
                i64::try_from(idx).unwrap_or(i64::MAX),
                track.video_id.as_str(),
                track_json,
                detail.tracks.next_cursor.as_deref(),
                &ctx.provider_version,
                now_i64,
                ttl_i64
            ],
        )?;
    }

    Ok(())
}

pub fn record_recent_playback_tx(
    tx: &Transaction<'_>,
    account_scope: &AccountScope,
    account_epoch: &AccountEpoch,
    entry: &RecentPlaybackRecord,
    now_ms: u64,
) -> Result<(), StorageError> {
    ensure_account_epoch_writable_tx(tx, account_scope, account_epoch, now_ms)?;
    let track_json = serde_json::to_string(&entry.track)?;
    let playback_ref_json = entry
        .playback_ref
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let recorded_at = i64::try_from(entry.recorded_at_ms).unwrap_or(i64::MAX);
    let position_ms = i64::try_from(entry.position_ms).unwrap_or(i64::MAX);
    let duration_ms = entry
        .duration_ms
        .map(|d| i64::try_from(d).unwrap_or(i64::MAX));

    tx.execute(
        r#"
INSERT INTO recent_playback
    (account_scope, track_json, playback_ref_json, position_ms, duration_ms, recorded_at_ms, source_context)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
"#,
        params![
            account_scope.as_str(),
            track_json,
            playback_ref_json,
            position_ms,
            duration_ms,
            recorded_at,
            entry.source_context.as_deref()
        ],
    )?;
    tx.execute(
        r#"
DELETE FROM recent_playback
WHERE account_scope = ?1 AND id NOT IN (
    SELECT id FROM recent_playback WHERE account_scope = ?1
    ORDER BY recorded_at_ms DESC, id DESC LIMIT ?2
)
"#,
        params![account_scope.as_str(), MAX_RECENT_PLAYBACK_PER_ACCOUNT],
    )?;
    Ok(())
}

/// Updates the newest history entry's position if it is still the given track.
pub fn update_latest_playback_position_tx(
    tx: &Transaction<'_>,
    account_scope: &AccountScope,
    account_epoch: &AccountEpoch,
    video_id: &VideoId,
    position_ms: u64,
    now_ms: u64,
) -> Result<(), StorageError> {
    ensure_account_epoch_writable_tx(tx, account_scope, account_epoch, now_ms)?;
    let latest: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, track_json FROM recent_playback WHERE account_scope = ?1 ORDER BY recorded_at_ms DESC, id DESC LIMIT 1",
            params![account_scope.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, track_json)) = latest else {
        return Ok(());
    };
    let track: Track = serde_json::from_str(&track_json)?;
    if &track.video_id == video_id {
        tx.execute(
            "UPDATE recent_playback SET position_ms = ?1 WHERE id = ?2",
            params![i64::try_from(position_ms).unwrap_or(i64::MAX), id],
        )?;
    }
    Ok(())
}

/// Scope of the most recently registered, still active account epoch (logout invalidates it).
pub fn last_active_account_scope_conn(
    conn: &Connection,
) -> Result<Option<AccountScope>, StorageError> {
    let scope: Option<String> = conn
        .query_row(
            "SELECT account_scope FROM account_epochs WHERE state = 'active' AND account_scope != '' ORDER BY updated_at_ms DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    scope
        .map(|s| {
            AccountScope::new(s)
                .map_err(|err| StorageError::Storage(format!("invalid stored scope: {err}")))
        })
        .transpose()
}

pub fn list_recent_playback_conn(
    conn: &Connection,
    account_scope: &AccountScope,
    limit: usize,
) -> Result<Vec<RecentPlaybackRecord>, StorageError> {
    Ok(list_recent_playback_with_ids(conn, account_scope, limit)?
        .into_iter()
        .map(|(_, record)| record)
        .collect())
}

pub(crate) fn list_recent_playback_with_ids(
    conn: &Connection,
    account_scope: &AccountScope,
    limit: usize,
) -> Result<Vec<(i64, RecentPlaybackRecord)>, StorageError> {
    let limit_i64 = i64::try_from(limit.max(1)).unwrap_or(100);
    let mut stmt = conn.prepare(
        r#"
SELECT id, track_json, playback_ref_json, position_ms, duration_ms, recorded_at_ms, source_context
FROM recent_playback
WHERE account_scope = ?1
ORDER BY recorded_at_ms DESC, id DESC
LIMIT ?2
"#,
    )?;
    let rows = stmt
        .query_map(params![account_scope.as_str(), limit_i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, track_json, playback_ref_json, pos_i64, dur_i64, rec_i64, source_context) in rows {
        let track: Track = serde_json::from_str(&track_json)?;
        let playback_ref: Option<PlaybackRef> = playback_ref_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        out.push((
            id,
            RecentPlaybackRecord {
                track,
                playback_ref,
                position_ms: u64::try_from(pos_i64.max(0)).unwrap_or(0),
                duration_ms: dur_i64.map(|d| u64::try_from(d.max(0)).unwrap_or(0)),
                recorded_at_ms: u64::try_from(rec_i64.max(0)).unwrap_or(0),
                source_context,
            },
        ));
    }
    Ok(out)
}

pub fn put_lyrics_tx(
    tx: &Transaction<'_>,
    cache_key: &str,
    provider: &str,
    result: &LyricsResult,
    ttl_secs: Option<u64>,
    now_ms: u64,
) -> Result<(), StorageError> {
    let default_ttl = match result.kind {
        LyricsKind::Synced | LyricsKind::Plain | LyricsKind::Instrumental => {
            DEFAULT_LYRICS_HIT_TTL_SECS
        }
        LyricsKind::NotFound => DEFAULT_LYRICS_NEGATIVE_TTL_SECS,
        LyricsKind::Unavailable | LyricsKind::Disabled => {
            // Never poison cache with transient network failures or disabled status.
            return Ok(());
        }
    };

    let effective_ttl = ttl_secs.unwrap_or(default_ttl);
    let digest = hex::encode(Sha256::digest(cache_key.as_bytes()));
    let kind_str = serde_json::to_string(&result.kind)?;
    let result_json = serde_json::to_string(result)?;
    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let ttl_i64 = i64::try_from(effective_ttl).unwrap_or(i64::MAX);

    tx.execute(
        r#"
INSERT INTO lyrics_cache (cache_key, video_id, provider, kind, result_json, fetched_at_ms, ttl_secs)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
ON CONFLICT(cache_key) DO UPDATE SET
    kind = excluded.kind,
    result_json = excluded.result_json,
    fetched_at_ms = excluded.fetched_at_ms,
    ttl_secs = excluded.ttl_secs
"#,
        params![
            digest,
            result.video_id.as_str(),
            provider,
            kind_str,
            result_json,
            now_i64,
            ttl_i64
        ],
    )?;
    Ok(())
}

pub fn get_lyrics_conn(
    conn: &Connection,
    cache_key: &str,
    now_ms: u64,
) -> Result<Option<CachedValue<LyricsResult>>, StorageError> {
    let digest = hex::encode(Sha256::digest(cache_key.as_bytes()));
    let row: Option<(String, i64, i64, String)> = conn
        .query_row(
            "SELECT result_json, fetched_at_ms, ttl_secs, provider FROM lyrics_cache WHERE cache_key = ?1",
            params![digest],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    let Some((result_json, fetched_i64, ttl_i64, provider)) = row else {
        return Ok(None);
    };
    let fetched_at_ms = u64::try_from(fetched_i64.max(0)).unwrap_or(0);
    let ttl_secs = u64::try_from(ttl_i64.max(0)).unwrap_or(0);
    let mut parsed: LyricsResult = serde_json::from_str(&result_json)?;
    parsed.user_offset_ms = get_lyrics_user_offset_conn(conn, &parsed.video_id)?;

    let freshness = evaluate_freshness(fetched_at_ms, ttl_secs, now_ms);
    let global_scope = AccountScope::new("global")
        .map_err(|err| StorageError::Storage(format!("invalid global scope: {err}")))?;

    Ok(Some(CachedValue {
        value: parsed,
        account_scope: global_scope,
        locale: String::new(),
        provider_version: provider,
        fetched_at_ms,
        ttl_secs,
        freshness,
    }))
}

pub fn get_lyrics_user_offset_conn(
    conn: &Connection,
    video_id: &VideoId,
) -> Result<i64, StorageError> {
    let offset: Option<i64> = conn
        .query_row(
            "SELECT user_offset_ms FROM lyrics_user_offsets WHERE video_id = ?1",
            params![video_id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(offset.unwrap_or(0))
}

pub fn set_lyrics_user_offset_tx(
    tx: &Transaction<'_>,
    video_id: &VideoId,
    offset_ms: i64,
    now_ms: u64,
) -> Result<(), StorageError> {
    let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
    tx.execute(
        r#"
INSERT INTO lyrics_user_offsets (video_id, user_offset_ms, updated_at_ms)
VALUES (?1, ?2, ?3)
ON CONFLICT(video_id) DO UPDATE SET
    user_offset_ms = excluded.user_offset_ms,
    updated_at_ms = excluded.updated_at_ms
"#,
        params![video_id.as_str(), offset_ms, now_i64],
    )?;
    Ok(())
}

pub fn reset_lyrics_user_offset_tx(
    tx: &Transaction<'_>,
    video_id: &VideoId,
) -> Result<(), StorageError> {
    tx.execute(
        "DELETE FROM lyrics_user_offsets WHERE video_id = ?1",
        params![video_id.as_str()],
    )?;
    Ok(())
}

// Legacy cache rows did not record the requested limit, so each page kind has a new namespace.
pub(crate) const LIBRARY_PLAYLIST_PAGE_KIND: &str = "library_playlists_page_v2";
pub(crate) const PLAYLIST_DETAIL_PAGE_KIND: &str = "playlist_detail_page_v2";
pub(crate) const SEARCH_PAGE_KIND: &str = "search_page_v2";
pub(crate) const LIKED_SONGS_PAGE_KIND: &str = "liked_songs_page_v2";

pub(crate) fn invalidate_library_playlist_views_tx(
    tx: &Transaction<'_>,
    account_scope: &str,
    playlist_id: &str,
) -> Result<(), StorageError> {
    tx.execute(
        "DELETE FROM catalog_cache WHERE account_scope = ?1
         AND entry_kind IN (?2, 'library_playlists', 'home_feed')",
        params![account_scope, LIBRARY_PLAYLIST_PAGE_KIND],
    )?;
    tx.execute(
        "DELETE FROM playlists WHERE account_scope = ?1 AND playlist_id = ?2",
        params![account_scope, playlist_id],
    )?;
    Ok(())
}

pub(crate) fn catalog_page_cache_key(cursor: Option<&str>, limit: usize) -> String {
    serde_json::json!([limit, cursor]).to_string()
}

pub(crate) fn playlist_detail_cache_key(playlist_id: &PlaylistId, page_key: &str) -> String {
    serde_json::json!([playlist_id.as_str(), page_key]).to_string()
}

pub fn search_cache_key(query: &str, cursor: Option<&str>, limit: usize) -> String {
    serde_json::json!([query.trim().to_ascii_lowercase(), limit, cursor]).to_string()
}

pub fn album_cache_key(album_id: &AlbumId) -> String {
    album_id.as_str().to_string()
}

pub fn artist_cache_key(artist_id: &ArtistId) -> String {
    artist_id.as_str().to_string()
}

pub fn patch_paged_freshness<T>(page: &mut PagedResult<T>, freshness: Freshness) {
    page.freshness = freshness;
}

pub fn patch_home_freshness(feed: &mut HomeFeed, freshness: Freshness) {
    feed.freshness = freshness;
}

pub fn patch_search_freshness(res: &mut SearchResult, freshness: Freshness) {
    res.songs.freshness = freshness;
}

pub fn patch_playlist_detail_freshness(detail: &mut PlaylistDetail, freshness: Freshness) {
    detail.tracks.freshness = freshness;
}

pub fn ensure_detail_types_compiled(_album: Option<&AlbumDetail>, _artist: Option<&ArtistDetail>) {}
