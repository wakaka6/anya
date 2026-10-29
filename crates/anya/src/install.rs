//! `anya --install`: copies the running executable to `~/.local/bin/anya`.

use std::path::{Path, PathBuf};

/// What `--install` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Copied to the path.
    Copied(PathBuf),
    /// The running executable already is the installed one.
    AlreadyThere(PathBuf),
}

/// Why `--install` failed; untranslated, for the message beside the
/// catalog's `cli.install.failed`.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// No home directory.
    #[error("no home directory")]
    NoHome,
    /// The running executable could not be found.
    #[error("cannot find the running executable: {0}")]
    Exe(std::io::Error),
    /// Copying failed.
    #[error("cannot install to {path}: {source}")]
    Copy {
        /// The target.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
}

/// `~/.local/bin/anya`.
pub fn target() -> Result<PathBuf, InstallError> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".local/bin/anya"))
        .ok_or(InstallError::NoHome)
}

/// Installs the running executable at [`target`].
pub fn install() -> Result<Installed, InstallError> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(InstallError::Exe)?;
    copy_to(&exe, &target()?)
}

/// Copies `exe` to `target` atomically (temp file, mode 0755, rename),
/// creating the directory.
pub fn copy_to(exe: &Path, target: &Path) -> Result<Installed, InstallError> {
    if std::fs::canonicalize(target).is_ok_and(|t| t == exe) {
        return Ok(Installed::AlreadyThere(target.to_owned()));
    }
    let fail = |source| InstallError::Copy {
        path: target.to_owned(),
        source,
    };
    let dir = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(fail)?;
    let tmp = dir.join(format!(".anya-install.{}.tmp", std::process::id()));
    let copied = (|| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::copy(exe, &tmp)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, target)
    })();
    if copied.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    copied.map_err(fail)?;
    Ok(Installed::Copied(target.to_owned()))
}

/// Whether `dir` is one of the directories in `PATH`.
pub fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_executable_and_knows_when_it_is_already_there() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("anya-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("dir");
        let exe = d.join("built");
        std::fs::write(&exe, b"binary").expect("write");
        let target = d.join("bin").join("anya");
        assert_eq!(
            copy_to(&exe, &target).expect("copy"),
            Installed::Copied(target.clone())
        );
        assert_eq!(std::fs::read(&target).expect("read"), b"binary");
        let mode = std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
        let installed = std::fs::canonicalize(&target).expect("canon");
        assert_eq!(
            copy_to(&installed, &target).expect("again"),
            Installed::AlreadyThere(target.clone())
        );
        let left = std::fs::read_dir(d.join("bin")).expect("ls").count();
        assert_eq!(left, 1, "no temp file left");
    }
}
