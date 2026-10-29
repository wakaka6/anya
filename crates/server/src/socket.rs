//! Binding the Unix socket: owner-only, a stale socket replaced, a live one
//! or a non-socket path never touched.

use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};

use crate::server::ServerError;

/// How long an existing socket gets to answer before it is taken as live.
const PROBE: Duration = Duration::from_secs(1);

/// The socket file this server created, removed on stop only while it is
/// still the same file.
#[derive(Debug)]
pub(crate) struct SocketFile {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl SocketFile {
    pub(crate) fn remove(&self) {
        let same = std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.dev && m.ino() == self.ino);
        if same && let Err(e) = std::fs::remove_file(&self.path) {
            tracing::debug!(path = %self.path.display(), error = %e, "removing the socket failed");
        }
    }
}

/// Binds `path`, replacing a socket nobody listens on.
pub(crate) async fn bind(path: &Path) -> Result<(UnixListener, SocketFile), ServerError> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if !dir.is_dir() {
        return Err(ServerError::MissingDirectory(dir));
    }
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(listen_error(path, e)),
        Ok(meta) if !meta.file_type().is_socket() => {
            return Err(ServerError::NotASocket(path.to_path_buf()));
        }
        Ok(_) => {
            if is_live(path).await {
                return Err(ServerError::InUse(path.display().to_string()));
            }
            tracing::debug!(path = %path.display(), "replacing a stale socket");
            std::fs::remove_file(path).map_err(|e| listen_error(path, e))?;
        }
    }
    let listener = UnixListener::bind(path).map_err(|e| listen_error(path, e))?;
    let owned = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .and_then(|()| std::fs::symlink_metadata(path));
    match owned {
        Ok(meta) => Ok((
            listener,
            SocketFile {
                path: path.to_path_buf(),
                dev: meta.dev(),
                ino: meta.ino(),
            },
        )),
        Err(e) => {
            drop(listener);
            let _ = std::fs::remove_file(path);
            Err(ServerError::Permissions {
                path: path.to_path_buf(),
                source: e,
            })
        }
    }
}

/// Whether something answers on the socket. No answer within [`PROBE`]
/// counts as live: a busy instance is never unlinked from under itself.
async fn is_live(path: &Path) -> bool {
    match tokio::time::timeout(PROBE, UnixStream::connect(path)).await {
        Ok(Ok(_)) | Err(_) => true,
        Ok(Err(e)) => !matches!(
            e.kind(),
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
        ),
    }
}

fn listen_error(path: &Path, source: io::Error) -> ServerError {
    ServerError::Listen {
        address: format!("unix:{}", path.display()),
        source,
    }
}
