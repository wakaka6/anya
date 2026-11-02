//! Plugin icons. A package names its own in `anya.icon`: a file in the
//! package or a `data:image/…` URI, never a URL, so showing it reaches no
//! author's server. The registry keeps a released plugin's icon beside its
//! tarball, so the market can show it before install. [`Icons`] keeps every
//! icon on disk under a name made from its content, checked like a download.

use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::digest;

use crate::PluginError;
use crate::package::Package;
use crate::registry::{download, verify_integrity};

/// The largest icon Anya keeps.
pub const MAX_ICON_BYTES: usize = 1 << 20;

/// What an icon file is. Anything else is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    Webp,
    /// Windows icon.
    Ico,
    /// SVG, shown only as an image, so its scripts never run.
    Svg,
}

impl IconFormat {
    /// The format of `bytes`, from their content, not a name.
    pub fn sniff(bytes: &[u8]) -> Option<IconFormat> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(IconFormat::Png)
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some(IconFormat::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(IconFormat::Gif)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(IconFormat::Webp)
        } else if bytes.starts_with(&[0, 0, 1, 0]) {
            Some(IconFormat::Ico)
        } else if bytes[..bytes.len().min(1024)]
            .to_ascii_lowercase()
            .windows(4)
            .any(|w| w == b"<svg")
        {
            Some(IconFormat::Svg)
        } else {
            None
        }
    }

    /// Its MIME type.
    pub fn content_type(self) -> &'static str {
        match self {
            IconFormat::Png => "image/png",
            IconFormat::Jpeg => "image/jpeg",
            IconFormat::Gif => "image/gif",
            IconFormat::Webp => "image/webp",
            IconFormat::Ico => "image/x-icon",
            IconFormat::Svg => "image/svg+xml",
        }
    }
}

/// `bytes` when they are an icon Anya keeps: at most [`MAX_ICON_BYTES`], in
/// one of the [`IconFormat`]s. `what` names it in the error.
pub fn check_icon(what: &str, bytes: &[u8]) -> Result<IconFormat, PluginError> {
    let refused = |reason: &str| PluginError::Io {
        path: what.to_owned(),
        reason: reason.to_owned(),
    };
    if bytes.is_empty() {
        return Err(refused("the icon is empty"));
    }
    if bytes.len() > MAX_ICON_BYTES {
        return Err(refused("the icon is larger than 1 MiB"));
    }
    IconFormat::sniff(bytes)
        .ok_or_else(|| refused("the icon is not PNG, JPEG, GIF, WebP, ICO or SVG"))
}

/// The icon `package` names, checked; `None` when it names none.
pub fn package_icon(package: &Package) -> Result<Option<Vec<u8>>, PluginError> {
    let Some(icon) = package.anya.icon.as_deref() else {
        return Ok(None);
    };
    let bytes = icon_bytes(icon, |file| {
        let path = package.dir.join(file);
        std::fs::read(&path).map_err(|e| PluginError::io(&path, &e))
    })?;
    check_icon(icon, &bytes)?;
    Ok(Some(bytes))
}

/// The bytes `anya.icon` names: a `data:image/…` URI decoded, else the file
/// at that path inside the package, read with `read`.
pub(crate) fn icon_bytes(
    icon: &str,
    read: impl FnOnce(&Path) -> Result<Vec<u8>, PluginError>,
) -> Result<Vec<u8>, PluginError> {
    let refused = |reason: &str| PluginError::Io {
        path: "anya.icon".to_owned(),
        reason: reason.to_owned(),
    };
    let icon = icon.trim();
    if icon.len() >= 11 && icon[..11].eq_ignore_ascii_case("data:image/") {
        return data_uri(icon).ok_or_else(|| refused("it is not a readable data:image URI"));
    }
    if icon.contains("://") {
        return Err(refused(
            "it must be a file in the package or a data:image URI",
        ));
    }
    let inside = Path::new(icon)
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    let file: PathBuf = Path::new(icon)
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    if !inside || file.as_os_str().is_empty() {
        return Err(refused("it is not a file in the package"));
    }
    read(&file)
}

/// The bytes of a `data:` URI, base64 or percent-encoded; `None` for one
/// that does not decode or would be larger than an icon may be.
fn data_uri(uri: &str) -> Option<Vec<u8>> {
    let (head, body) = uri.split_once(',')?;
    if body.len() > MAX_ICON_BYTES * 4 / 3 + 4 {
        return None;
    }
    if head.to_ascii_lowercase().ends_with(";base64") {
        let compact: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        return STANDARD.decode(compact).ok();
    }
    let mut out = Vec::with_capacity(body.len());
    let mut bytes = body.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    Some(out)
}

/// Icons kept on disk, each under a name made from its SHA-512, so the same
/// picture is kept once and a name from the index is known before download.
#[derive(Debug, Clone)]
pub struct Icons {
    dir: PathBuf,
}

impl Icons {
    /// The icons in `dir`; nothing is created until the first is kept.
    pub fn new(dir: PathBuf) -> Icons {
        Icons { dir }
    }

    /// The name an icon with the integrity string `integrity`
    /// (`sha512-<base64>`) is kept under; `None` for another format.
    pub fn name_of(integrity: &str) -> Option<String> {
        let sum = STANDARD.decode(integrity.strip_prefix("sha512-")?).ok()?;
        (sum.len() == 64).then(|| hex(&sum[..16]))
    }

    /// Checks `bytes` and keeps them; the name to ask for them by.
    pub fn keep(&self, bytes: &[u8]) -> Result<String, PluginError> {
        check_icon("icon", bytes)?;
        let name = hex(&digest::digest(&digest::SHA512, bytes).as_ref()[..16]);
        let path = self.dir.join(&name);
        if !path.is_file() {
            std::fs::create_dir_all(&self.dir).map_err(|e| PluginError::io(&self.dir, &e))?;
            let partial = self.dir.join(format!(".{name}.partial"));
            std::fs::write(&partial, bytes).map_err(|e| PluginError::io(&partial, &e))?;
            std::fs::rename(&partial, &path).map_err(|e| PluginError::io(&path, &e))?;
        }
        Ok(name)
    }

    /// The icon kept as `name` and its format; `None` when there is none, or
    /// `name` is not one [`Icons::keep`] makes.
    pub fn read(&self, name: &str) -> Option<(Vec<u8>, IconFormat)> {
        let ok = name.len() == 32
            && name
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !ok {
            return None;
        }
        let bytes = std::fs::read(self.dir.join(name)).ok()?;
        let format = check_icon(name, &bytes).ok()?;
        Some((bytes, format))
    }

    /// Downloads the icon at `url`, checks it against `integrity` and keeps
    /// it; its name.
    pub async fn fetch(&self, url: &str, integrity: &str) -> Result<String, PluginError> {
        let bytes = download(url, MAX_ICON_BYTES).await?;
        verify_integrity(url, &bytes, integrity)?;
        self.keep(&bytes)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
