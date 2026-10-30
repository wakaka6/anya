//! Whether Homebrew has a newer Anya, and running the upgrade
//! (docs/decisions/0014). Anya never replaces its own binary: the version
//! comes from the tap's formula, the one `brew upgrade` would install.

use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

/// The default formula: the tap's `anya.rb`.
pub const DEFAULT_FORMULA: &str =
    "https://raw.githubusercontent.com/wakaka6/homebrew-tap/main/Formula/anya.rb";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// `brew upgrade` downloads and may build; it gets a generous but finite time.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// A formula is a few kilobytes; anything near this is not one.
const MAX_FORMULA_BYTES: usize = 256 * 1024;
/// The Homebrew prefixes searched when `brew` is not on `PATH`: Apple
/// Silicon, Intel macOS and Linuxbrew.
const BREW_PREFIXES: [&str; 3] = [
    "/opt/homebrew/bin/brew",
    "/usr/local/bin/brew",
    "/home/linuxbrew/.linuxbrew/bin/brew",
];

/// Why a check or an upgrade failed. The Display text is English, for logs
/// and for showing beside a translated message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// The formula could not be fetched or read.
    #[error("cannot fetch {url}: {reason}")]
    Fetch {
        /// What was asked for.
        url: String,
        /// The network's or the file system's own words.
        reason: String,
    },
    /// The tap has no formula yet: nothing has been released.
    #[error("{url} does not exist: no release has been published")]
    NotPublished {
        /// Where the formula was looked for.
        url: String,
    },
    /// The formula has no readable version.
    #[error("the formula at {url} names no version")]
    NoVersion {
        /// Where the formula was read.
        url: String,
    },
    /// This Anya was not installed by Homebrew.
    #[error("this Anya was not installed with Homebrew")]
    NotHomebrew,
    /// `brew` was not found.
    #[error("brew was not found")]
    BrewMissing,
    /// `brew upgrade` ran and failed.
    #[error("brew upgrade failed: {detail}")]
    Upgrade {
        /// The end of brew's own output.
        detail: String,
    },
}

/// How this Anya was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// Under a Homebrew Cellar, so `brew upgrade anya` updates it.
    Homebrew,
    /// Anything else: a copied binary, `cargo install`, a source build.
    Other,
}

impl Install {
    /// The install of the running executable; `Other` when it cannot be told.
    pub fn current() -> Install {
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_or(Install::Other, |exe| Install::of(&exe))
    }

    /// The install `exe` (symlinks resolved) belongs to: Homebrew keeps a
    /// formula's files in `<prefix>/Cellar/<formula>/<version>/`.
    pub fn of(exe: &Path) -> Install {
        let in_cellar = exe
            .components()
            .zip(exe.components().skip(1))
            .any(|(a, b)| a.as_os_str() == "Cellar" && b.as_os_str() == "anya");
        if in_cellar {
            Install::Homebrew
        } else {
            Install::Other
        }
    }
}

/// The version a formula declares: its `version "x.y.z"` line, else the
/// `vX.Y.Z` tag in its first `url`.
pub fn formula_version(formula: &str) -> Option<Version> {
    let quoted = |line: &str, key: &str| -> Option<String> {
        let rest = line.trim_start().strip_prefix(key)?.trim_start();
        let rest = rest.strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_owned())
    };
    let declared = formula
        .lines()
        .find_map(|line| quoted(line, "version"))
        .and_then(|v| Version::parse(v.trim_start_matches('v')).ok());
    declared.or_else(|| {
        formula
            .lines()
            .find_map(|line| quoted(line, "url"))
            .and_then(|url| {
                url.split('/').find_map(|part| {
                    let tag = part.strip_prefix('v')?;
                    Version::parse(tag).ok()
                })
            })
    })
}

/// Whether `latest` is newer than the running `current`. A `current` that
/// is not a version is never older than anything.
pub fn is_newer(current: &str, latest: &Version) -> bool {
    Version::parse(current).is_ok_and(|c| *latest > c)
}

/// The newest version the formula at `url` (`https://` or `file://`) names.
pub async fn latest(url: &str) -> Result<Version, UpdateError> {
    let fetch_err = |reason: String| UpdateError::Fetch {
        url: url.to_owned(),
        reason,
    };
    let parsed = reqwest::Url::parse(url).map_err(|e| fetch_err(e.to_string()))?;
    let text = match parsed.scheme() {
        "file" => {
            let path = parsed
                .to_file_path()
                .map_err(|()| fetch_err("not a local path".to_owned()))?;
            std::fs::read_to_string(path).map_err(|e| fetch_err(e.to_string()))?
        }
        "http" | "https" => {
            let client = reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT)
                .user_agent(concat!("anya/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| fetch_err(e.without_url().to_string()))?;
            let response = client
                .get(parsed)
                .send()
                .await
                .map_err(|e| fetch_err(e.without_url().to_string()))?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(UpdateError::NotPublished {
                    url: url.to_owned(),
                });
            }
            if !response.status().is_success() {
                return Err(fetch_err(format!("HTTP {}", response.status())));
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|e| fetch_err(e.without_url().to_string()))?;
            if bytes.len() > MAX_FORMULA_BYTES {
                return Err(fetch_err("too large for a formula".to_owned()));
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
        other => return Err(fetch_err(format!("unsupported scheme {other}"))),
    };
    formula_version(&text).ok_or_else(|| UpdateError::NoVersion {
        url: url.to_owned(),
    })
}

/// `brew`: on `PATH`, else in Homebrew's default prefixes.
pub fn find_brew() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("brew"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        BREW_PREFIXES
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    })
}

/// Runs `brew upgrade anya`. Only for a Homebrew install; the running
/// process keeps going on the old binary until it is restarted.
pub async fn upgrade() -> Result<(), UpdateError> {
    if Install::current() != Install::Homebrew {
        return Err(UpdateError::NotHomebrew);
    }
    let brew = find_brew().ok_or(UpdateError::BrewMissing)?;
    run_upgrade(&brew).await
}

async fn run_upgrade(brew: &Path) -> Result<(), UpdateError> {
    let run = tokio::process::Command::new(brew)
        .args(["upgrade", "anya"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(UPGRADE_TIMEOUT, run)
        .await
        .map_err(|_| UpdateError::Upgrade {
            detail: "timed out".to_owned(),
        })?
        .map_err(|e| UpdateError::Upgrade {
            detail: e.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = text.lines().rev().take(5).collect();
    let detail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    Err(UpdateError::Upgrade {
        detail: if detail.trim().is_empty() {
            output.status.to_string()
        } else {
            detail
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMULA: &str = r#"class Anya < Formula
  desc "Local speech-to-text"
  on_macos do
    on_arm do
      url "https://github.com/wakaka6/anya/releases/download/v0.3.1/anya_darwin_arm64.tar.gz"
      sha256 "abc"
    end
  end
  version "0.3.1"
end
"#;

    #[test]
    fn the_declared_version_wins_and_the_url_tag_is_the_fallback() {
        assert_eq!(formula_version(FORMULA), Version::parse("0.3.1").ok());
        let no_version = FORMULA.replace("  version \"0.3.1\"\n", "");
        assert_eq!(formula_version(&no_version), Version::parse("0.3.1").ok());
        assert_eq!(formula_version("class Anya < Formula\nend\n"), None);
    }

    #[test]
    fn only_a_higher_version_is_news() {
        let v = |s: &str| Version::parse(s).expect("version");
        assert!(is_newer("0.3.0", &v("0.3.1")));
        assert!(!is_newer("0.3.1", &v("0.3.1")));
        assert!(!is_newer("0.4.0", &v("0.3.1")));
        assert!(!is_newer("not a version", &v("0.3.1")));
    }

    #[test]
    fn a_cellar_path_is_homebrew_and_a_look_alike_is_not() {
        let home = |p| Install::of(Path::new(p));
        assert_eq!(
            home("/opt/homebrew/Cellar/anya/0.3.1/bin/anya"),
            Install::Homebrew
        );
        assert_eq!(
            home("/home/linuxbrew/.linuxbrew/Cellar/anya/0.3.1/bin/anya"),
            Install::Homebrew
        );
        assert_eq!(
            home("/opt/homebrew/Cellar/anyone/1/bin/anya"),
            Install::Other
        );
        assert_eq!(home("/Users/me/.cargo/bin/anya"), Install::Other);
        assert_eq!(home("/Users/me/Cellar-notes/anya"), Install::Other);
    }

    #[tokio::test]
    async fn the_latest_version_is_read_from_a_formula_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("anya.rb");
        std::fs::write(&path, FORMULA).expect("write");
        let url = format!("file://{}", path.display());
        assert_eq!(latest(&url).await.ok(), Version::parse("0.3.1").ok());

        std::fs::write(&path, "class Anya < Formula\nend\n").expect("write");
        assert!(matches!(
            latest(&url).await,
            Err(UpdateError::NoVersion { .. })
        ));
        let missing = format!("file://{}", dir.path().join("none.rb").display());
        assert!(matches!(
            latest(&missing).await,
            Err(UpdateError::Fetch { .. })
        ));
        assert!(matches!(
            latest("ftp://example.com/anya.rb").await,
            Err(UpdateError::Fetch { .. })
        ));
    }

    #[tokio::test]
    async fn a_missing_formula_means_nothing_is_published_but_a_server_error_is_a_failure() {
        use std::io::{Read, Write};
        let serve = |status: &'static str| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let url = format!("http://{}/anya.rb", listener.local_addr().expect("addr"));
            std::thread::spawn(move || {
                if let Ok((mut conn, _)) = listener.accept() {
                    let mut buf = [0; 1024];
                    let _ = conn.read(&mut buf);
                    let _ = write!(
                        conn,
                        "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    );
                }
            });
            url
        };
        assert!(matches!(
            latest(&serve("404 Not Found")).await,
            Err(UpdateError::NotPublished { .. })
        ));
        assert!(matches!(
            latest(&serve("500 Internal Server Error")).await,
            Err(UpdateError::Fetch { .. })
        ));
    }

    #[cfg(unix)]
    fn fake_brew(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("brew");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_upgrade_runs_brew_with_upgrade_anya_and_reports_its_failure() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = dir.path().join("args");
        let ok = fake_brew(dir.path(), &format!("echo \"$@\" > {}", seen.display()));
        assert_eq!(run_upgrade(&ok).await, Ok(()));
        assert_eq!(
            std::fs::read_to_string(&seen).expect("args").trim(),
            "upgrade anya"
        );

        let failing = fake_brew(dir.path(), "echo 'Error: anya not installed' >&2; exit 1");
        assert_eq!(
            run_upgrade(&failing).await,
            Err(UpdateError::Upgrade {
                detail: "Error: anya not installed".to_owned()
            })
        );
    }
}
