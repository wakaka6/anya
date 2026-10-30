//! Owner-only files: the discovery file, the paired clients and the TLS
//! identity are written the same way.

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Creates `dir` and its missing parents, owner-only (0700).
pub(crate) fn create_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Writes `contents` to `path`, owner-only (0600) and atomically, creating
/// its directory (0700) when missing.
pub(crate) fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_dir(dir)?;
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
            f.write_all(contents)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}
