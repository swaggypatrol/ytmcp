-- Synthetic v1 schema and seed fixture for migration + online backup verification.
CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY,
    applied_at_ms INTEGER NOT NULL,
    description TEXT NOT NULL
);

CREATE TABLE account_epochs (
    account_epoch TEXT PRIMARY KEY,
    account_scope TEXT NOT NULL,
    state TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE playlists (
    account_scope TEXT NOT NULL,
    locale TEXT NOT NULL,
    playlist_id TEXT NOT NULL,
    browse_id TEXT,
    title TEXT NOT NULL,
    subtitle TEXT,
    track_count INTEGER,
    cover_json TEXT,
    position_idx INTEGER NOT NULL,
    provider_version TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL,
    ttl_secs INTEGER NOT NULL,
    PRIMARY KEY (account_scope, locale, playlist_id)
);

CREATE TABLE playlist_tracks (
    account_scope TEXT NOT NULL,
    locale TEXT NOT NULL,
    playlist_id TEXT NOT NULL,
    cursor_key TEXT NOT NULL,
    position_idx INTEGER NOT NULL,
    video_id TEXT NOT NULL,
    track_json TEXT NOT NULL,
    next_cursor TEXT,
    provider_version TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL,
    ttl_secs INTEGER NOT NULL,
    PRIMARY KEY (account_scope, locale, playlist_id, cursor_key, position_idx)
);

CREATE TABLE catalog_cache (
    account_scope TEXT NOT NULL,
    locale TEXT NOT NULL,
    entry_kind TEXT NOT NULL,
    cache_key TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL,
    ttl_secs INTEGER NOT NULL,
    PRIMARY KEY (account_scope, locale, entry_kind, cache_key)
);

CREATE TABLE recent_playback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_scope TEXT NOT NULL,
    track_json TEXT NOT NULL,
    playback_ref_json TEXT,
    position_ms INTEGER NOT NULL,
    duration_ms INTEGER,
    recorded_at_ms INTEGER NOT NULL
);

CREATE TABLE lyrics_cache (
    cache_key TEXT PRIMARY KEY,
    video_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL,
    result_json TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL,
    ttl_secs INTEGER NOT NULL
);

CREATE TABLE lyrics_user_offsets (
    video_id TEXT PRIMARY KEY,
    user_offset_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE cover_assets (
    asset_id TEXT PRIMARY KEY,
    rel_path TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    accent_oklch TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    last_accessed_ms INTEGER NOT NULL
);

CREATE TABLE cover_url_map (
    source_url TEXT PRIMARY KEY,
    asset_id TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL
);

INSERT INTO schema_version (version, applied_at_ms, description)
VALUES (1, 1700000000000, 'initial v1 synthetic fixture');

INSERT INTO account_epochs (account_epoch, account_scope, state, updated_at_ms)
VALUES ('epoch-v1-alpha', 'scope-alpha', 'active', 1700000000000);

INSERT INTO recent_playback (account_scope, track_json, playback_ref_json, position_ms, duration_ms, recorded_at_ms)
VALUES (
    'scope-alpha',
    '{"videoId":"vid_legacy_1","title":"Legacy Track","artists":[{"name":"Synthetic Artist"}],"availability":"available"}',
    NULL,
    42000,
    180000,
    1700000000500
);

INSERT INTO lyrics_user_offsets (video_id, user_offset_ms, updated_at_ms)
VALUES ('vid_legacy_1', -250, 1700000000500);
