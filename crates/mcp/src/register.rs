//! Local administrative registration. This CLI is intentionally outside the MCP tool router.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use ytm_control_api::local::{
    AccountScope, ClientCredential, MAX_GRANTS, NEVER_EXPIRES_MS, Registration, load_private_json,
    validate_private_dir,
};
use ytm_control_api::{GrantAction, PlaylistId};

const USAGE: &str = "Usage: ytmcp register --principal ID [--account-scope SCOPE|signed-out] [--grant dj|record|edit|playback|queue|rating|artist-subscription|library-membership|local-history|preferences|create-private-playlist|append-playlist|edit-playlist|delete-playlist|account-handoff] [--playlist PLAYLIST_ID] [--hours 1..168]";
const SIGN_IN_FIRST: &str = "no signed-in YouTube Music account was found in the player; sign in in the player first, or pass --account-scope signed-out for playback-only control";
const PRIVACY_REFUSED: &str = "playlist privacy is not grantable: only the listener changes it, in the player or on the YouTube Music website";
const DEFAULT_EDIT_HOURS: u64 = 24;

// Device settings and lyrics offsets stay on this machine, so they belong to the play tier.
const PLAY_TIER: &[GrantAction] = &[
    GrantAction::Dj,
    GrantAction::Playback,
    GrantAction::Queue,
    GrantAction::Preferences,
];
const RECORD_TIER: &[GrantAction] = &[
    GrantAction::Rating,
    GrantAction::LibraryMembership,
    GrantAction::ArtistSubscription,
    GrantAction::CreatePrivatePlaylist,
];
/// Actions that can lose the listener's data; a registration holding any of them is time-limited.
const EDIT_TIER: &[GrantAction] = &[
    GrantAction::AddPlaylistEntries,
    GrantAction::EditPlaylist,
    GrantAction::DeletePlaylist,
    GrantAction::LocalHistory,
];

struct Request {
    principal: String,
    account_scope: Option<String>,
    actions: Vec<GrantAction>,
    playlists: Vec<PlaylistId>,
    hours: Option<u64>,
}

/// `ytmcp register ...`: writes `control-credentials/<principal>.json` and prints its path.
pub fn register() -> Result<(), String> {
    let request = parse(std::env::args().skip(2))?;
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid system clock")?
            .as_millis(),
    )
    .map_err(|_| "invalid system clock")?;
    let path = register_in(request, &config_dir()?, &database_file()?, now)?;
    println!("{}", path.display());
    Ok(())
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Request, String> {
    let mut args = args.into_iter();
    let mut principal = None;
    let mut account_scope = None;
    let mut actions = Vec::new();
    let mut playlists = Vec::new();
    let mut hours = None;
    while let Some(option) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        match option.as_str() {
            "--principal" if principal.is_none() => principal = Some(value),
            "--account-scope" if account_scope.is_none() => account_scope = Some(value),
            "--hours" if hours.is_none() => {
                hours = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|x| (1..=168).contains(x))
                        .ok_or(USAGE)?,
                )
            }
            // Grants name playlists by their playlist ID; accept the VL browse form as the same one.
            "--playlist" => playlists.push(
                PlaylistId::new(value.strip_prefix("VL").unwrap_or(&value))
                    .map_err(|_| "invalid playlist ID")?,
            ),
            "--grant" => {
                for action in grant_actions(&value)? {
                    if !actions.contains(action) {
                        actions.push(*action);
                    }
                }
            }
            _ => return Err(USAGE.to_owned()),
        }
    }
    let principal = principal
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or(USAGE)?;
    Ok(Request {
        principal,
        account_scope,
        actions,
        playlists,
        hours,
    })
}

/// Tier names (`dj`, `record`, `edit`) and the fine-grained action names they expand to.
fn grant_actions(name: &str) -> Result<&'static [GrantAction], String> {
    Ok(match name {
        "dj" => PLAY_TIER,
        "record" => RECORD_TIER,
        "edit" => EDIT_TIER,
        "playlist-privacy" => return Err(PRIVACY_REFUSED.to_owned()),
        "playback" => &[GrantAction::Playback],
        "queue" => &[GrantAction::Queue],
        "rating" => &[GrantAction::Rating],
        "artist-subscription" => &[GrantAction::ArtistSubscription],
        "library-membership" => &[GrantAction::LibraryMembership],
        "local-history" => &[GrantAction::LocalHistory],
        "preferences" => &[GrantAction::Preferences],
        "create-private-playlist" => &[GrantAction::CreatePrivatePlaylist],
        "append-playlist" => &[GrantAction::AddPlaylistEntries],
        "edit-playlist" => &[GrantAction::EditPlaylist],
        "delete-playlist" => &[GrantAction::DeletePlaylist],
        "account-handoff" => &[GrantAction::AccountHandoff],
        _ => return Err(USAGE.to_owned()),
    })
}

/// Registers `request` under `config`, replacing any earlier registration of the same principal.
/// Without `--account-scope`, the player's active account is read from `database`.
fn register_in(
    request: Request,
    config: &Path,
    database: &Path,
    now: u64,
) -> Result<PathBuf, String> {
    let Request {
        principal,
        account_scope,
        actions,
        playlists,
        hours,
    } = request;
    let account_scope = match account_scope.as_deref() {
        Some("signed-out") => None,
        Some(scope) => Some(
            serde_json::from_value::<AccountScope>(serde_json::Value::String(scope.to_owned()))
                .map_err(|_| "invalid account scope")?,
        ),
        None => Some(active_account_scope(database)?.ok_or(SIGN_IN_FIRST)?),
    };
    if account_scope.is_none()
        && actions.iter().any(|action| {
            !matches!(
                action,
                GrantAction::Playback
                    | GrantAction::Queue
                    | GrantAction::Preferences
                    | GrantAction::AccountHandoff
            )
        })
    {
        return Err("cloud operations require a named account scope".to_owned());
    }
    if actions.iter().any(|action| {
        matches!(
            action,
            GrantAction::AddPlaylistEntries
                | GrantAction::EditPlaylist
                | GrantAction::DeletePlaylist
        )
    }) != !playlists.is_empty()
    {
        return Err("playlist editing grants require specific --playlist IDs".to_owned());
    }
    let hours = if actions.iter().any(|action| EDIT_TIER.contains(action)) {
        Some(hours.unwrap_or(DEFAULT_EDIT_HOURS))
    } else {
        hours
    };
    let expires_at_ms = hours.map_or(NEVER_EXPIRES_MS, |hours| now + hours * 60 * 60 * 1000);

    let credentials = config.join("control-credentials");
    create_private_dir(config)
        .and_then(|_| create_private_dir(&credentials))
        .map_err(|err| err.to_string())?;
    let lock = open_lock(&config.join("control-clients.lock")).map_err(|err| err.to_string())?;
    lock.try_lock()
        .map_err(|_| "another music control registration is being updated")?;
    let registrations = config.join("control-clients.json");
    let mut records: Vec<Registration> = match load_private_json(&registrations) {
        Ok(records) => records,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err.to_string()),
    };
    records.retain(|record| record.expires_at_ms > now && record.principal != principal);
    if records.len() >= MAX_GRANTS {
        return Err("the registration list is full".to_owned());
    }
    let token = hex::encode(rand::random::<[u8; 32]>());
    let token_sha256 = hex::encode(Sha256::digest(token.as_bytes()));
    let credential = ClientCredential {
        principal: principal.clone(),
        token,
    };
    let path = credentials.join(format!("{principal}.json"));
    records.push(Registration {
        principal,
        account_scope,
        token_sha256,
        expires_at_ms,
        actions,
        editable_playlists: playlists,
    });

    // Both files are complete on disk before either replaces its predecessor.
    let staged_registrations = temporary_in(config, "control-clients");
    let staged_credential = temporary_in(&credentials, "credential");
    let publish = write_private_json(&staged_registrations, &records)
        .and_then(|_| write_private_json(&staged_credential, &credential))
        .and_then(|_| fs::rename(&staged_registrations, &registrations))
        .and_then(|_| fs::rename(&staged_credential, &path));
    if let Err(err) = publish {
        let _ = fs::remove_file(&staged_registrations);
        let _ = fs::remove_file(&staged_credential);
        return Err(err.to_string());
    }
    for dir in [&credentials, config] {
        File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|err| err.to_string())?;
    }
    Ok(path)
}

/// The account scope of the player's newest active account epoch, read without modifying the
/// player's database. When `ytmcp.db` does not exist yet (upgraded install registered before
/// first launch), falls back read-only to a sibling legacy `ytm-glass/ytm-glass.db`.
fn active_account_scope(database: &Path) -> Result<Option<AccountScope>, String> {
    use rusqlite::{Connection, OpenFlags, OptionalExtension};

    let candidate = if database.exists() {
        database.to_path_buf()
    } else if let Some(data_dir) = database.parent()
        && let Some(base_data_dir) = data_dir.parent()
    {
        let sibling = base_data_dir.join("ytm-glass").join("ytm-glass.db");
        if sibling.exists() {
            sibling
        } else {
            return Ok(None);
        }
    } else {
        return Ok(None);
    };
    let unreadable = |_| "the player's database could not be read".to_owned();
    let conn = Connection::open_with_flags(
        candidate,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(unreadable)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(unreadable)?;
    let scope: Option<String> = conn
        .query_row(
            "SELECT account_scope FROM account_epochs WHERE state = 'active' AND account_scope != '' ORDER BY updated_at_ms DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(unreadable)?;
    scope
        .map(|scope| {
            serde_json::from_value(serde_json::Value::String(scope))
                .map_err(|_| "the player's database holds an invalid account scope".to_owned())
        })
        .transpose()
}

/// `YTMCP_DATA_DIR`, resolved the same way as the daemon's storage root.
fn data_root() -> Option<PathBuf> {
    std::env::var("YTMCP_DATA_DIR")
        .ok()
        .map(|root| root.trim().to_owned())
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
}

fn config_dir() -> Result<PathBuf, String> {
    if let Some(root) = data_root() {
        return Ok(root.join("config/ytmcp"));
    }
    directories::BaseDirs::new()
        .map(|base| base.config_dir().join("ytmcp"))
        .ok_or_else(|| "the user configuration directory is unavailable".into())
}

fn database_file() -> Result<PathBuf, String> {
    let data = match data_root() {
        Some(root) => root.join("data/ytmcp"),
        None => directories::BaseDirs::new()
            .map(|base| base.data_dir().join("ytmcp"))
            .ok_or("the user data directory is unavailable")?,
    };
    Ok(data.join("ytmcp.db"))
}

fn temporary_in(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!(".{stem}.{:016x}.tmp", rand::random::<u64>()))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    validate_private_dir(path)
}

fn write_private_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()
}

fn open_lock(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    use std::os::unix::fs::MetadataExt;

    let file = File::from(open(
        path,
        OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid control registration lock",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    const HOUR_MS: u64 = 60 * 60 * 1000;

    struct Fixture {
        _root: tempfile::TempDir,
        config: PathBuf,
        database: PathBuf,
    }

    impl Fixture {
        /// A private root with a player database whose newest active account is `active`.
        fn new(active: Option<&str>) -> Self {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join("config");
            let database = root.path().join("ytmcp.db");
            let conn = rusqlite::Connection::open(&database).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE account_epochs (account_epoch TEXT PRIMARY KEY, account_scope TEXT NOT NULL, state TEXT NOT NULL, updated_at_ms INTEGER NOT NULL);
                 INSERT INTO account_epochs VALUES ('e-old', 'scope-older', 'active', 10);
                 INSERT INTO account_epochs VALUES ('e-gone', 'scope-signed-out', 'invalidated', 90);
                 INSERT INTO account_epochs VALUES ('e-blank', '', 'active', 95);",
            )
            .unwrap();
            if let Some(scope) = active {
                conn.execute(
                    "INSERT INTO account_epochs VALUES ('e-new', ?1, 'active', 50)",
                    [scope],
                )
                .unwrap();
            } else {
                conn.execute("DELETE FROM account_epochs WHERE state = 'active'", [])
                    .unwrap();
            }
            Self {
                _root: root,
                config,
                database,
            }
        }

        fn register(&self, args: &[&str]) -> Result<PathBuf, String> {
            let request = parse(args.iter().map(|arg| (*arg).to_owned()))?;
            register_in(request, &self.config, &self.database, NOW)
        }

        fn records(&self) -> Vec<Registration> {
            load_private_json(&self.config.join("control-clients.json")).unwrap()
        }

        fn only(&self) -> Registration {
            let mut records = self.records();
            assert_eq!(records.len(), 1);
            records.pop().unwrap()
        }
    }

    #[test]
    fn the_account_scope_defaults_to_the_players_newest_active_account() {
        let fixture = Fixture::new(Some("scope-current"));
        fixture
            .register(&["--principal", "pi", "--grant", "dj"])
            .unwrap();
        assert_eq!(
            fixture.only().account_scope.unwrap().as_str(),
            "scope-current"
        );

        let explicit = Fixture::new(None);
        explicit
            .register(&["--principal", "pi", "--account-scope", "scope-explicit"])
            .unwrap();
        assert_eq!(
            explicit.only().account_scope.unwrap().as_str(),
            "scope-explicit"
        );
        explicit
            .register(&["--principal", "pi", "--account-scope", "signed-out"])
            .unwrap();
        assert!(explicit.only().account_scope.is_none());
    }

    #[test]
    fn without_a_signed_in_player_the_default_account_asks_to_sign_in() {
        let fixture = Fixture::new(None);
        let error = fixture
            .register(&["--principal", "pi", "--grant", "dj"])
            .unwrap_err();
        assert_eq!(error, SIGN_IN_FIRST);
        fs::remove_file(&fixture.database).unwrap();
        let error = fixture
            .register(&["--principal", "pi", "--grant", "dj"])
            .unwrap_err();
        assert_eq!(error, SIGN_IN_FIRST);
        assert!(!fixture.database.exists());
    }

    #[test]
    fn tier_names_expand_to_their_actions() {
        let fixture = Fixture::new(Some("scope-current"));
        fixture
            .register(&[
                "--principal",
                "pi",
                "--grant",
                "dj",
                "--grant",
                "record",
                "--grant",
                "edit",
                "--grant",
                "queue",
                "--playlist",
                "VLPL_fixture",
            ])
            .unwrap();
        let record = fixture.only();
        let expected: Vec<_> = [PLAY_TIER, RECORD_TIER, EDIT_TIER].concat();
        assert_eq!(record.actions, expected);
        assert_eq!(record.editable_playlists[0].as_str(), "PL_fixture");

        let error = fixture
            .register(&["--principal", "pi", "--grant", "edit"])
            .unwrap_err();
        assert!(error.contains("--playlist"), "{error}");
    }

    #[test]
    fn only_edit_grants_expire() {
        let fixture = Fixture::new(Some("scope-current"));
        for tiers in [&["dj"][..], &["dj", "record"]] {
            let mut args = vec!["--principal", "pi"];
            for tier in tiers {
                args.extend(["--grant", tier]);
            }
            fixture.register(&args).unwrap();
            assert_eq!(fixture.only().expires_at_ms, NEVER_EXPIRES_MS);
        }
        fixture
            .register(&["--principal", "pi", "--grant", "dj", "--hours", "2"])
            .unwrap();
        assert_eq!(fixture.only().expires_at_ms, NOW + 2 * HOUR_MS);
        fixture
            .register(&[
                "--principal",
                "pi",
                "--grant",
                "dj",
                "--grant",
                "edit",
                "--playlist",
                "PL_fixture",
            ])
            .unwrap();
        assert_eq!(fixture.only().expires_at_ms, NOW + 24 * HOUR_MS);
        fixture
            .register(&[
                "--principal",
                "pi",
                "--grant",
                "local-history",
                "--hours",
                "168",
            ])
            .unwrap();
        assert_eq!(fixture.only().expires_at_ms, NOW + 168 * HOUR_MS);
        assert!(
            fixture
                .register(&[
                    "--principal",
                    "pi",
                    "--grant",
                    "local-history",
                    "--hours",
                    "169"
                ])
                .is_err()
        );
    }

    #[test]
    fn registering_again_keeps_the_credential_path_and_replaces_the_token() {
        let fixture = Fixture::new(Some("scope-current"));
        let first = fixture
            .register(&["--principal", "pi", "--grant", "dj"])
            .unwrap();
        let old: ClientCredential = load_private_json(&first).unwrap();
        fixture
            .register(&["--principal", "claude", "--grant", "dj"])
            .unwrap();
        let second = fixture
            .register(&["--principal", "pi", "--grant", "dj", "--grant", "record"])
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first, fixture.config.join("control-credentials/pi.json"));
        let new: ClientCredential = load_private_json(&second).unwrap();
        assert_ne!(old.token, new.token);
        let records = fixture.records();
        assert_eq!(records.len(), 2);
        let pi = records.iter().find(|r| r.principal == "pi").unwrap();
        assert_eq!(pi.token_sha256, hex::encode(Sha256::digest(&new.token)));
        assert_eq!(pi.actions, [PLAY_TIER, RECORD_TIER].concat());
        // Only the two credentials remain; no staged file is left behind.
        assert_eq!(
            fs::read_dir(fixture.config.join("control-credentials"))
                .unwrap()
                .count(),
            2
        );
    }

    #[test]
    fn the_registration_limit_counts_distinct_clients() {
        let fixture = Fixture::new(Some("scope-current"));
        for index in 0..MAX_GRANTS {
            fixture
                .register(&["--principal", &format!("client-{index}"), "--grant", "dj"])
                .unwrap();
        }
        fixture
            .register(&["--principal", "client-0", "--grant", "dj"])
            .unwrap();
        assert!(
            fixture
                .register(&["--principal", "one-more", "--grant", "dj"])
                .unwrap_err()
                .contains("full")
        );
    }

    #[test]
    fn playlist_privacy_is_never_granted() {
        let fixture = Fixture::new(Some("scope-current"));
        let error = fixture
            .register(&[
                "--principal",
                "pi",
                "--grant",
                "playlist-privacy",
                "--playlist",
                "PL_fixture",
            ])
            .unwrap_err();
        assert_eq!(error, PRIVACY_REFUSED);
        assert!(!fixture.config.join("control-clients.json").exists());
    }

    #[test]
    fn registration_falls_back_read_only_to_legacy_database_before_first_launch() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_dir = dir.path().join("data/ytm-glass");
        let current_db = dir.path().join("data/ytmcp/ytmcp.db");
        fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_db = legacy_dir.join("ytm-glass.db");
        let conn = rusqlite::Connection::open(&legacy_db).unwrap();
        conn.execute(
            "CREATE TABLE account_epochs (account_epoch TEXT PRIMARY KEY, account_scope TEXT NOT NULL, state TEXT NOT NULL, updated_at_ms INTEGER NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO account_epochs VALUES ('ep-1', 'legacy-scope', 'active', 100)",
            [],
        )
        .unwrap();
        drop(conn);

        let scope = active_account_scope(&current_db).unwrap();
        assert_eq!(scope.as_ref().map(|s| s.as_str()), Some("legacy-scope"));
    }
}
