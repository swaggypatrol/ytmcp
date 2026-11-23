use crate::error::StorageError;
use crate::paths::{StoragePaths, ensure_dir_0700, set_file_0600};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tracing::{info, warn};
use ytm_core::ports::Clock;

pub const LATEST_SCHEMA_VERSION: u32 = 7;

struct Migration {
    version: u32,
    description: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "initial storage schema with account epochs, catalog, lyrics, and covers",
        sql: r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at_ms INTEGER NOT NULL,
    description TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS account_epochs (
    account_epoch TEXT PRIMARY KEY,
    account_scope TEXT NOT NULL,
    state TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_account_epochs_scope_state
    ON account_epochs (account_scope, state);

CREATE TABLE IF NOT EXISTS playlists (
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

CREATE TABLE IF NOT EXISTS playlist_tracks (
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

CREATE TABLE IF NOT EXISTS catalog_cache (
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

CREATE TABLE IF NOT EXISTS recent_playback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_scope TEXT NOT NULL,
    track_json TEXT NOT NULL,
    playback_ref_json TEXT,
    position_ms INTEGER NOT NULL,
    duration_ms INTEGER,
    recorded_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS lyrics_cache (
    cache_key TEXT PRIMARY KEY,
    video_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL,
    result_json TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL,
    ttl_secs INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS lyrics_user_offsets (
    video_id TEXT PRIMARY KEY,
    user_offset_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS cover_assets (
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

CREATE TABLE IF NOT EXISTS cover_url_map (
    source_url TEXT PRIMARY KEY,
    asset_id TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL
);
"#,
    },
    Migration {
        version: 2,
        description: "add performance indices, cover format extension, and playback source context",
        sql: r#"
CREATE INDEX IF NOT EXISTS idx_recent_playback_scope_time
    ON recent_playback (account_scope, recorded_at_ms DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_cover_assets_lru
    ON cover_assets (last_accessed_ms ASC, created_at_ms ASC, asset_id ASC);
CREATE INDEX IF NOT EXISTS idx_lyrics_cache_video
    ON lyrics_cache (video_id, provider);
ALTER TABLE cover_assets ADD COLUMN format_ext TEXT NOT NULL DEFAULT '';
ALTER TABLE recent_playback ADD COLUMN source_context TEXT;
"#,
    },
    Migration {
        version: 3,
        description: "add durable control operation journal",
        sql: r#"
CREATE TABLE control_operations (
    principal TEXT NOT NULL,
    account_scope TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    payload_hash BLOB NOT NULL CHECK (length(payload_hash) = 32),
    revision INTEGER NOT NULL,
    settled INTEGER NOT NULL CHECK (settled IN (0, 1)),
    updated_at_ms INTEGER NOT NULL,
    request_json TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (principal, account_scope, operation_id)
);
CREATE INDEX idx_control_operations_unsettled
    ON control_operations (settled) WHERE settled = 0;
"#,
    },
    Migration {
        version: 4,
        description: "persist DJ plans and session revisions",
        sql: r#"
CREATE TABLE control_dj_plans (
    account_scope TEXT NOT NULL,
    principal TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    plan_json TEXT NOT NULL,
    PRIMARY KEY (account_scope, principal, plan_id)
);
CREATE TABLE control_dj_sessions (
    account_scope TEXT NOT NULL,
    principal TEXT NOT NULL,
    session_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    session_json TEXT NOT NULL,
    PRIMARY KEY (account_scope, principal, session_id)
);
"#,
    },
    Migration {
        version: 5,
        description: "index recent desktop control activity by account",
        sql: "CREATE INDEX idx_control_operations_account_recent ON control_operations (account_scope, updated_at_ms);",
    },
    Migration {
        version: 6,
        description: "persist account DJ preferences by scope and subject",
        sql: r#"
CREATE TABLE control_dj_preferences (
    account_scope TEXT NOT NULL,
    preference_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    preference_json TEXT NOT NULL,
    PRIMARY KEY (account_scope, preference_id),
    UNIQUE (account_scope, session_id, subject_kind, subject_id)
);
"#,
    },
    Migration {
        version: 7,
        description: "record when each control operation was claimed, for daily account limits",
        sql: r#"
ALTER TABLE control_operations ADD COLUMN claimed_at_ms INTEGER NOT NULL DEFAULT 0;
UPDATE control_operations SET claimed_at_ms = updated_at_ms;
CREATE INDEX idx_control_operations_claimed
    ON control_operations (account_scope, claimed_at_ms);
"#,
    },
];

pub fn open_and_migrate(
    paths: &StoragePaths,
    clock: &dyn Clock,
    target_max_version: u32,
) -> Result<(Connection, Vec<std::path::PathBuf>), StorageError> {
    if target_max_version > LATEST_SCHEMA_VERSION {
        return Err(StorageError::Storage(format!(
            "requested schema version {target_max_version} exceeds latest {LATEST_SCHEMA_VERSION}"
        )));
    }

    ensure_dir_0700(&paths.data_dir)?;
    let db_path = paths.database_file();
    let existed = db_path.exists();
    let pre_len = if existed {
        fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Open read-write explicitly. If the existing DB is read-only or corrupt,
    // we fail with StorageError and NEVER delete or overwrite the user's DB.
    let mut conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|err| StorageError::Storage(format!("failed to open sqlite database: {err}")))?;

    if !existed {
        set_file_0600(&db_path)?;
    }

    if existed && pre_len > 0 {
        verify_integrity(&conn)?;
    }

    conn.busy_timeout(Duration::from_millis(5_000))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;\nPRAGMA journal_mode = WAL;\nPRAGMA synchronous = NORMAL;",
    )
    .map_err(|err| {
        StorageError::Storage(format!(
            "failed to configure sqlite pragmas (read-only or invalid): {err}"
        ))
    })?;

    let current_version = detect_schema_version(&conn)?;
    if current_version > LATEST_SCHEMA_VERSION {
        return Err(StorageError::Storage(format!(
            "database schema version {current_version} is newer than supported {LATEST_SCHEMA_VERSION}"
        )));
    }

    let mut created_backups = Vec::new();
    if current_version < target_max_version {
        if existed && pre_len > 0 {
            let backup_path = unique_backup_path(paths, current_version, target_max_version, clock);
            perform_online_backup(&conn, &backup_path)?;
            created_backups.push(backup_path);
        }

        for migration in MIGRATIONS {
            if migration.version > current_version && migration.version <= target_max_version {
                let now_ms = i64::try_from(clock.wall_now_ms()).unwrap_or(i64::MAX);
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                tx.execute_batch(migration.sql)?;
                tx.execute(
                    "INSERT INTO schema_version (version, applied_at_ms, description) VALUES (?1, ?2, ?3)",
                    params![migration.version, now_ms, migration.description],
                )?;
                tx.commit()?;
                info!(
                    version = migration.version,
                    description = migration.description,
                    "applied sqlite schema migration"
                );
            }
        }
    }

    Ok((conn, created_backups))
}

fn verify_integrity(conn: &Connection) -> Result<(), StorageError> {
    let mut stmt = conn.prepare("PRAGMA quick_check").map_err(|err| {
        warn!("sqlite integrity check preparation failed: {err}");
        StorageError::Storage(
            "sqlite database appears corrupted; original file preserved".to_string(),
        )
    })?;
    let mut rows = stmt.query([]).map_err(|err| {
        warn!("sqlite integrity check query failed: {err}");
        StorageError::Storage(
            "sqlite database appears corrupted; original file preserved".to_string(),
        )
    })?;

    let mut all_ok = false;
    while let Some(row) = rows.next().map_err(|err| {
        warn!("sqlite integrity check row read failed: {err}");
        StorageError::Storage(
            "sqlite database appears corrupted; original file preserved".to_string(),
        )
    })? {
        let status: String = row.get(0).map_err(|err| {
            StorageError::Storage(format!("invalid sqlite integrity output: {err}"))
        })?;
        if status.eq_ignore_ascii_case("ok") {
            all_ok = true;
        } else {
            return Err(StorageError::Storage(
                "sqlite database failed integrity check; original file preserved".to_string(),
            ));
        }
    }

    if !all_ok {
        return Err(StorageError::Storage(
            "sqlite database integrity check returned no status; original file preserved"
                .to_string(),
        ));
    }
    Ok(())
}

fn detect_schema_version(conn: &Connection) -> Result<u32, StorageError> {
    let has_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    if has_table == 0 {
        return Ok(0);
    }
    let max_version: Option<u32> =
        conn.query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })?;
    Ok(max_version.unwrap_or(0))
}

fn unique_backup_path(
    paths: &StoragePaths,
    from_version: u32,
    to_version: u32,
    clock: &dyn Clock,
) -> std::path::PathBuf {
    let base_ts = clock.wall_now_ms();
    let candidate = paths.backup_file(from_version, to_version, base_ts);
    if !candidate.exists() {
        return candidate;
    }
    for suffix in 1u64..10_000 {
        let alt = paths.data_dir.join(format!(
            "ytm-glass.db.v{from_version}-to-v{to_version}.{base_ts}.{suffix}.bak"
        ));
        if !alt.exists() {
            return alt;
        }
    }
    candidate
}

fn perform_online_backup(src: &Connection, backup_path: &Path) -> Result<(), StorageError> {
    let mut dst = Connection::open_with_flags(
        backup_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|err| StorageError::Storage(format!("failed to create sqlite backup file: {err}")))?;

    let backup_res = (|| -> Result<(), StorageError> {
        let backup = rusqlite::backup::Backup::new(src, &mut dst)?;
        backup.run_to_completion(128, Duration::from_millis(5), None)?;
        Ok(())
    })();

    drop(dst);

    if let Err(err) = backup_res {
        let _ = fs::remove_file(backup_path);
        return Err(err);
    }

    set_file_0600(backup_path)?;
    info!(backup = %backup_path.display(), "created sqlite online backup prior to migration");
    Ok(())
}
