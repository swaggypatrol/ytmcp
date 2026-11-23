//! Trusted local socket transport types, excluded from the music-tool schema.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LinesCodec};
pub use ytm_protocol::AccountScope;

use crate::{ControlError, GrantAction, MAX_FRAME_BYTES};

pub const MAX_GRANTS: usize = 16;
pub const TOKEN_BYTES: usize = 64;
/// `expires_at_ms` of a registration that stays valid until it is replaced or removed.
pub const NEVER_EXPIRES_MS: u64 = u64::MAX;
const MAX_CONFIG_BYTES: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientHello {
    pub control_version: u32,
    pub principal: String,
    pub token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerHello {
    pub control_version: u32,
    pub error: Option<ControlError>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Registration {
    pub principal: String,
    pub token_sha256: String,
    pub account_scope: Option<AccountScope>,
    pub expires_at_ms: u64,
    #[serde(default)]
    pub actions: Vec<GrantAction>,
    #[serde(default)]
    pub editable_playlists: Vec<ytm_protocol::PlaylistId>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientCredential {
    pub principal: String,
    pub token: String,
}

pub fn auto_socket_path() -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR is required for local music control"))?;
    validate_private_dir(&dir)?;
    Ok(dir.join("ytm-glass").join("control.sock"))
}

pub fn validate_private_dir(path: &Path) -> io::Result<()> {
    let facts = fs::symlink_metadata(path)?;
    let uid = rustix::process::geteuid().as_raw();
    if !path.is_absolute()
        || !facts.is_dir()
        || facts.uid() != uid
        || facts.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "music control directory must be an absolute owned 0700 directory",
        ));
    }
    Ok(())
}

pub fn validate_socket(path: &Path) -> io::Result<fs::Metadata> {
    let facts = fs::symlink_metadata(path)?;
    if !facts.file_type().is_socket()
        || facts.uid() != rustix::process::geteuid().as_raw()
        || facts.permissions().mode() & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "music control socket must be an owned 0600 socket",
        ));
    }
    Ok(facts)
}

pub fn validate_peer(stream: &UnixStream) -> io::Result<()> {
    if stream.peer_cred()?.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "music control requires a peer from this user account",
        ));
    }
    Ok(())
}

pub fn load_private_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    use rustix::fs::{Mode, OFlags, open};

    let mut file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let facts = file.metadata()?;
    if !facts.is_file()
        || facts.uid() != rustix::process::geteuid().as_raw()
        || facts.permissions().mode() & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control configuration must be an owned regular 0600 file",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control configuration is oversized",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid control configuration"))
}

pub fn framed(stream: UnixStream) -> Framed<UnixStream, LinesCodec> {
    Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES))
}

pub async fn send<T: Serialize>(
    frames: &mut Framed<UnixStream, LinesCodec>,
    value: &T,
) -> io::Result<()> {
    let line = serde_json::to_string(value).map_err(io::Error::other)?;
    if line.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control response exceeds frame budget",
        ));
    }
    frames.send(line).await.map_err(io::Error::other)
}

pub async fn receive<T: DeserializeOwned>(
    frames: &mut Framed<UnixStream, LinesCodec>,
) -> io::Result<Option<T>> {
    let Some(line) = frames.next().await else {
        return Ok(None);
    };
    let line = line.map_err(io::Error::other)?;
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid control frame"))
}
