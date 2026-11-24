use crate::error::StorageError;
use directories::BaseDirs;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// XDG application directory name from `config/app-identity.json` (`xdgDirName`).
pub const APP_XDG_DIR_NAME: &str = "ytmcp";

/// Database file name of installs from before the project was renamed from YTM Glass to YTMcp.
const LEGACY_DATABASE_NAME: &str = "ytm-glass.db";

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Resolved XDG directories for settings, SQLite database, and cover cache.
///
/// The cover cache lives under `$XDG_CACHE_HOME/ytmcp/covers/`, strictly separate
/// from `$XDG_CONFIG_HOME/ytmcp/` and `$XDG_DATA_HOME/ytmcp/` so clearing cache
/// never deletes settings, database profile metadata, or logs out the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl StoragePaths {
    pub fn new(config_dir: PathBuf, data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
            cache_dir,
        }
    }

    /// Resolves standard XDG directories using `directories::BaseDirs` for `ytmcp`.
    pub fn from_xdg() -> Result<Self, StorageError> {
        let base = BaseDirs::new().ok_or_else(|| {
            StorageError::Storage("unable to resolve XDG base directories".to_string())
        })?;
        Ok(Self {
            config_dir: base.config_dir().join(APP_XDG_DIR_NAME),
            data_dir: base.data_dir().join(APP_XDG_DIR_NAME),
            cache_dir: base.cache_dir().join(APP_XDG_DIR_NAME),
        })
    }

    /// Creates an isolated directory layout under `root` for tests.
    pub fn for_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            config_dir: root.join("config").join(APP_XDG_DIR_NAME),
            data_dir: root.join("data").join(APP_XDG_DIR_NAME),
            cache_dir: root.join("cache").join(APP_XDG_DIR_NAME),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    pub fn moment_config_file(&self) -> PathBuf {
        self.config_dir.join("moment-config.json")
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join("ytmcp.db")
    }

    /// Renames a database left by an install from before the rename, with its WAL and shared
    /// memory files, to the current name. Does nothing when the current database exists or
    /// there is no legacy one; returns whether it renamed. Must run before the database is
    /// opened. The side files move first, so an interrupted rename never strands a WAL.
    pub fn adopt_legacy_database(&self) -> Result<bool, StorageError> {
        let current = self.database_file();
        let legacy = self.data_dir.join(LEGACY_DATABASE_NAME);
        if current.exists() || !legacy.exists() {
            return Ok(false);
        }
        for suffix in ["-wal", "-shm"] {
            let from = with_suffix(&legacy, suffix);
            if from.exists() {
                fs::rename(&from, with_suffix(&current, suffix))?;
            }
        }
        fs::rename(&legacy, &current)?;
        Ok(true)
    }

    pub fn covers_dir(&self) -> PathBuf {
        self.cache_dir.join("covers")
    }

    pub fn backup_file(&self, from_version: u32, to_version: u32, timestamp_ms: u64) -> PathBuf {
        self.data_dir.join(format!(
            "ytmcp.db.v{from_version}-to-v{to_version}.{timestamp_ms}.bak"
        ))
    }

    pub fn ensure_layout(&self) -> Result<(), StorageError> {
        ensure_dir_0700(&self.config_dir)?;
        ensure_dir_0700(&self.data_dir)?;
        ensure_dir_0700(&self.cache_dir)?;
        ensure_dir_0700(&self.covers_dir())?;
        Ok(())
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

pub fn ensure_dir_0700(dir: &Path) -> Result<(), StorageError> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    set_dir_0700(dir)?;
    Ok(())
}

pub fn set_dir_0700(dir: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(dir, perms)?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

pub fn set_file_0600(path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Atomically writes `bytes` to `target` via a `0600` temp file in the same directory + rename.
pub fn write_atomic_0600(target: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let parent = target
        .parent()
        .ok_or_else(|| StorageError::Storage("target path has no parent directory".to_string()))?;
    ensure_dir_0700(parent)?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp_path = parent.join(format!(".{file_name}.tmp.{pid}.{seq}"));

    let write_res = (|| -> Result<(), StorageError> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);

        let mut file = opts.open(&tmp_path)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        set_file_0600(&tmp_path)?;
        fs::rename(&tmp_path, target)?;
        set_file_0600(target)?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}
