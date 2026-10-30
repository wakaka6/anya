//! The Bun runtime that runs plugins: the one on `PATH` when it is recent
//! enough, else a pinned release downloaded into the runtime directory. The
//! download happens only when the user installs a plugin
//! (docs/decisions/0004-plugins-in-bun.md); without plugins Bun is never fetched.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ring::digest;

use crate::PluginError;
use crate::registry::download;

/// The oldest Bun the host runs on.
pub const MIN_BUN_VERSION: &str = "1.2.0";

/// The Bun release Anya downloads.
pub const PINNED_BUN_VERSION: &str = "1.3.14";

/// Where Bun's releases are published.
const RELEASES_URL: &str = "https://github.com/oven-sh/bun/releases/download";

/// SHA-256 of each platform's zip in the pinned release, from its
/// `SHASUMS256.txt`.
const PINNED_SHA256: &[(&str, &str)] = &[
    (
        "bun-darwin-aarch64",
        "d8b96221828ad6f97ac7ac0ab7e95872341af763001e8803e8267652c2652620",
    ),
    (
        "bun-darwin-x64",
        "4183df3374623e5bab315c547cfa0974533cd457d86b73b639f7a87974cd6633",
    ),
    (
        "bun-linux-aarch64",
        "a27ffb63a8310375836e0d6f668ae17fa8d8d18b88c37c821c65331973a19a3b",
    ),
    (
        "bun-linux-x64",
        "951ee2aee855f08595aeec6225226a298d3fea83a3dcd6465c09cbccdf7e848f",
    ),
];

/// The largest Bun zip Anya downloads.
const MAX_ZIP_BYTES: usize = 200 << 20;

/// How long `bun --version` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// A Bun executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BunPath(pub PathBuf);

/// A Bun release to download: where it is and which hash it must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BunRelease {
    /// The release's version; names the directory it unpacks into.
    pub version: String,
    /// URL of the platform's zip.
    pub url: String,
    /// Lowercase hex SHA-256 the zip must have.
    pub sha256: String,
}

impl BunRelease {
    /// The pinned release for this platform. Errors on a platform Bun has
    /// no release for in the table.
    pub fn pinned() -> Result<BunRelease, PluginError> {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            "linux" => "linux",
            other => return Err(PluginError::Bun(format!("operating system {other}"))),
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            "x86_64" => "x64",
            other => return Err(PluginError::Bun(format!("architecture {other}"))),
        };
        let asset = format!("bun-{os}-{arch}");
        let sha256 = PINNED_SHA256
            .iter()
            .find(|(name, _)| *name == asset)
            .map(|(_, sha)| (*sha).to_owned())
            .ok_or_else(|| PluginError::Bun(asset.clone()))?;
        Ok(BunRelease {
            version: PINNED_BUN_VERSION.to_owned(),
            url: format!("{RELEASES_URL}/bun-v{PINNED_BUN_VERSION}/{asset}.zip"),
            sha256,
        })
    }
}

impl BunPath {
    /// A usable Bun that needs no download: the first `bun` on `PATH` that
    /// is [`MIN_BUN_VERSION`] or newer, else one downloaded earlier into
    /// `runtime`.
    pub async fn detect(runtime: &Path) -> Option<BunPath> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("bun");
            if candidate.is_file() && version_ok(&candidate).await {
                return Some(BunPath(candidate));
            }
        }
        let release = BunRelease::pinned().ok()?;
        let downloaded = downloaded_path(runtime, &release);
        downloaded.is_file().then_some(BunPath(downloaded))
    }

    /// A usable Bun: [`BunPath::detect`], else the pinned release downloaded
    /// into `runtime`. Call it when the user installs their first plugin.
    pub async fn provision(runtime: &Path) -> Result<BunPath, PluginError> {
        if let Some(bun) = BunPath::detect(runtime).await {
            return Ok(bun);
        }
        BunPath::download(runtime, &BunRelease::pinned()?).await
    }

    /// Downloads `release` into `runtime`, checks its SHA-256 and unpacks the
    /// `bun` executable from it.
    pub async fn download(runtime: &Path, release: &BunRelease) -> Result<BunPath, PluginError> {
        let zip = download(&release.url, MAX_ZIP_BYTES).await?;
        let actual = hex(digest::digest(&digest::SHA256, &zip).as_ref());
        if actual != release.sha256 {
            return Err(PluginError::Integrity {
                what: release.url.clone(),
                expected: release.sha256.clone(),
                actual,
            });
        }
        let target = downloaded_path(runtime, release);
        let (url, dest) = (release.url.clone(), target.clone());
        tokio::task::spawn_blocking(move || unzip_bun(&url, &zip, &dest))
            .await
            .map_err(|e| PluginError::Bun(e.to_string()))??;
        Ok(BunPath(target))
    }
}

fn downloaded_path(runtime: &Path, release: &BunRelease) -> PathBuf {
    runtime.join("bun").join(&release.version).join("bun")
}

async fn version_ok(bun: &Path) -> bool {
    let mut command = tokio::process::Command::new(bun);
    command.arg("--version").kill_on_drop(true);
    let Ok(Ok(output)) = tokio::time::timeout(PROBE_TIMEOUT, command.output()).await else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let (Ok(found), Ok(min)) = (
        semver::Version::parse(text.trim()),
        semver::Version::parse(MIN_BUN_VERSION),
    ) else {
        return false;
    };
    output.status.success() && found >= min
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes the zip's `bun` executable to `dest`.
fn unzip_bun(what: &str, zip: &[u8], dest: &Path) -> Result<(), PluginError> {
    use std::os::unix::fs::PermissionsExt as _;

    let archive_err = |reason: String| PluginError::Archive {
        what: what.to_owned(),
        reason,
    };
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| archive_err(e.to_string()))?;
    let mut exe = None;
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| archive_err(e.to_string()))?;
        if file.is_file()
            && file
                .enclosed_name()
                .and_then(|n| n.file_name().map(|f| f == "bun"))
                == Some(true)
        {
            exe = Some(i);
            break;
        }
    }
    let index = exe.ok_or_else(|| archive_err("it has no bun executable".to_owned()))?;
    let mut file = archive
        .by_index(index)
        .map_err(|e| archive_err(e.to_string()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| archive_err(e.to_string()))?;

    let dir = dest
        .parent()
        .ok_or_else(|| archive_err("no target directory".to_owned()))?;
    std::fs::create_dir_all(dir).map_err(|e| PluginError::io(dir, &e))?;
    let partial = dest.with_extension("partial");
    std::fs::write(&partial, &bytes).map_err(|e| PluginError::io(&partial, &e))?;
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| PluginError::io(&partial, &e))?;
    std::fs::rename(&partial, dest).map_err(|e| PluginError::io(dest, &e))
}
