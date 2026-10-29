//! The discovery file: where this instance listens, so an app finds it
//! instead of guessing a port (docs/decisions/0007).

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A discovery file this server wrote, removed on stop only while it still
/// holds what was written: another instance may have replaced it.
#[derive(Debug)]
pub(crate) struct DiscoveryFile {
    path: PathBuf,
    contents: Vec<u8>,
}

impl DiscoveryFile {
    /// Writes `{url, version, protocol, pid}` to `path`, owner-only and
    /// atomically, creating its directory (0700) when missing.
    pub(crate) fn write(
        path: &Path,
        url: &str,
        version: &str,
        protocol: u32,
    ) -> io::Result<DiscoveryFile> {
        let json = serde_json::json!({
            "url": url,
            "version": version,
            "protocol": protocol,
            "pid": std::process::id(),
        });
        let mut contents = json.to_string().into_bytes();
        contents.push(b'\n');
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{}.tmp", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let written = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| {
                f.set_permissions(Permissions::from_mode(0o600))?;
                f.write_all(&contents)?;
                f.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(DiscoveryFile {
            path: path.to_path_buf(),
            contents,
        })
    }

    pub(crate) fn remove(&self) {
        if std::fs::read(&self.path).is_ok_and(|c| c == self.contents)
            && let Err(e) = std::fs::remove_file(&self.path)
        {
            tracing::debug!(path = %self.path.display(), error = %e, "removing the discovery file failed");
        }
    }
}
