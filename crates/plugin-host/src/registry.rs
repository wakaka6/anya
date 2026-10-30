//! The anya-plugins registry: fetching its index, downloading a release
//! and checking that the bytes are the ones the index promised
//! (docs/decisions/0010-anya-plugins-registry.md).

use std::io::Read as _;
use std::path::{Component, Path};
use std::time::Duration;

use anya_protocol::{INDEX_VERSION, Index, IndexVersion};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::digest;

use crate::PluginError;

/// Where the registry publishes its index. Callers may pass another URL to
/// [`Registry::fetch`]: a mirror, a local file for plugin development.
pub const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/wakaka6/anya-plugins/dist/index.json";

/// The largest index Anya reads.
const MAX_INDEX_BYTES: usize = 4 << 20;

/// The largest release tarball Anya downloads.
pub(crate) const MAX_ARTIFACT_BYTES: usize = 64 << 20;

/// The most a tarball may unpack to; guards against decompression bombs.
const MAX_UNPACKED_BYTES: u64 = 256 << 20;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// The registry. Reading it changes nothing locally; installing is
/// [`crate::Store::install`].
pub struct Registry;

impl Registry {
    /// Downloads and parses the index at `index_url` (`https://` or
    /// `file://`). An index of another format version is an error.
    pub async fn fetch(index_url: &str) -> Result<Index, PluginError> {
        let bytes = download(index_url, MAX_INDEX_BYTES).await?;
        parse_index(&bytes)
    }
}

/// Parses an index and checks its format version.
pub fn parse_index(bytes: &[u8]) -> Result<Index, PluginError> {
    let index: Index =
        serde_json::from_slice(bytes).map_err(|e| PluginError::Index(e.to_string()))?;
    if index.version != INDEX_VERSION {
        return Err(PluginError::IndexVersion {
            found: index.version,
            supported: INDEX_VERSION,
        });
    }
    Ok(index)
}

/// Finds a release in `index`.
pub fn find_release<'a>(
    index: &'a Index,
    id: &str,
    version: &str,
) -> Result<&'a IndexVersion, PluginError> {
    index
        .plugins
        .iter()
        .filter(|p| p.id == id)
        .flat_map(|p| &p.versions)
        .find(|v| v.version == version)
        .ok_or_else(|| PluginError::NotInIndex {
            id: id.to_owned(),
            version: version.to_owned(),
        })
}

/// `sha512-<base64>` of `bytes`, the integrity string the index carries.
pub fn integrity_of(bytes: &[u8]) -> String {
    format!(
        "sha512-{}",
        STANDARD.encode(digest::digest(&digest::SHA512, bytes))
    )
}

/// Checks that `bytes` hash to `expected` (`sha512-<base64>`). `what` names
/// the artifact in the error.
pub fn verify_integrity(what: &str, bytes: &[u8], expected: &str) -> Result<(), PluginError> {
    if !expected.starts_with("sha512-") {
        return Err(PluginError::IntegrityFormat(expected.to_owned()));
    }
    let actual = integrity_of(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(PluginError::Integrity {
            what: what.to_owned(),
            expected: expected.to_owned(),
            actual,
        })
    }
}

/// The bytes at `url`, at most `limit`. `https://` and `http://` go over the
/// network, `file://` reads a local file.
pub(crate) async fn download(url: &str, limit: usize) -> Result<Vec<u8>, PluginError> {
    let fetch_err = |reason: String| PluginError::Fetch {
        url: url.to_owned(),
        reason,
    };
    let parsed = reqwest::Url::parse(url).map_err(|e| fetch_err(e.to_string()))?;
    let bytes = match parsed.scheme() {
        "file" => {
            let path = parsed
                .to_file_path()
                .map_err(|()| fetch_err("not a local path".to_owned()))?;
            tokio::fs::read(&path)
                .await
                .map_err(|e| fetch_err(e.to_string()))?
        }
        "http" | "https" => {
            let client = reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT)
                .user_agent(concat!("anya-plugin-host/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| fetch_err(e.without_url().to_string()))?;
            let mut response = client
                .get(parsed)
                .send()
                .await
                .map_err(|e| fetch_err(e.without_url().to_string()))?;
            if !response.status().is_success() {
                return Err(fetch_err(format!("HTTP {}", response.status())));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| fetch_err(e.without_url().to_string()))?
            {
                if body.len() + chunk.len() > limit {
                    return Err(fetch_err(format!("larger than {limit} bytes")));
                }
                body.extend_from_slice(&chunk);
            }
            body
        }
        other => return Err(fetch_err(format!("scheme {other:?} is not supported"))),
    };
    if bytes.len() > limit {
        return Err(fetch_err(format!("larger than {limit} bytes")));
    }
    Ok(bytes)
}

/// Unpacks an npm tarball into the existing, empty `dest`. The leading
/// directory (`package/`) is dropped. Only files and directories are
/// accepted, and no path may leave `dest`.
pub(crate) fn unpack_tarball(what: &str, tgz: &[u8], dest: &Path) -> Result<(), PluginError> {
    let archive_err = |reason: String| PluginError::Archive {
        what: what.to_owned(),
        reason,
    };
    let gz = flate2::read::GzDecoder::new(tgz).take(MAX_UNPACKED_BYTES + 1);
    let mut archive = tar::Archive::new(gz);
    let mut unpacked = 0u64;
    for entry in archive.entries().map_err(|e| archive_err(e.to_string()))? {
        let mut entry = entry.map_err(|e| archive_err(e.to_string()))?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| archive_err(e.to_string()))?
            .into_owned();
        let mut parts = path.components();
        parts.next();
        let relative = parts.as_path();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(archive_err(format!(
                "{} leaves the package",
                path.display()
            )));
        }
        let target = dest.join(relative);
        if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| PluginError::io(&target, &e))?;
        } else if kind.is_file() {
            unpacked += entry.size();
            if unpacked > MAX_UNPACKED_BYTES {
                return Err(archive_err(format!(
                    "unpacks to more than {MAX_UNPACKED_BYTES} bytes"
                )));
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| PluginError::io(parent, &e))?;
            }
            let mut file =
                std::fs::File::create(&target).map_err(|e| PluginError::io(&target, &e))?;
            std::io::copy(&mut entry, &mut file).map_err(|e| archive_err(e.to_string()))?;
        } else {
            return Err(archive_err(format!(
                "{} is a link or special file",
                path.display()
            )));
        }
    }
    if !dest.join("package.json").is_file() {
        return Err(archive_err("it has no package.json".to_owned()));
    }
    Ok(())
}
