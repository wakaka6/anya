//! The discovery file: where this instance listens, so an app finds it
//! instead of guessing a port (docs/decisions/0007).

use std::io;
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
        crate::private::write(path, &contents)?;
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
