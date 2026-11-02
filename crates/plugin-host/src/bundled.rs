//! Plugins built into Anya: packages whose files are compiled into the
//! binary. The market lists them without the registry and they install from
//! the binary, through the same checks and unpacking as a download
//! (docs/decisions/0017-official-plugins-built-in.md).

use std::io::Write;
use std::path::Path;

use anya_protocol::{IndexPlugin, IndexVersion, PLUGIN_PROTOCOL, Provide};

use crate::PluginError;
use crate::package::parse;
use crate::registry::integrity_of;

/// A plugin package built into Anya.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bundled {
    /// What its entries export. Hooks only: an engine built into Anya is
    /// written in Rust.
    pub provides: &'static [Provide],
    /// Its files by path inside the package, `package.json` among them.
    pub files: &'static [(&'static str, &'static str)],
}

impl Bundled {
    /// The plugin as a registry entry with one release, so the market,
    /// `anya plugin search` and the install checks read it like any other.
    pub fn entry(&self) -> Result<IndexPlugin, PluginError> {
        Ok(self.release()?.0)
    }

    /// The entry and the package as an npm tarball.
    pub(crate) fn release(&self) -> Result<(IndexPlugin, Vec<u8>), PluginError> {
        let json = self
            .files
            .iter()
            .find(|(path, _)| *path == "package.json")
            .map(|(_, text)| *text)
            .ok_or_else(|| PluginError::Io {
                path: "package.json".to_owned(),
                reason: "a built-in plugin has no package.json".to_owned(),
            })?;
        let package = parse(json.as_bytes(), "package.json", Path::new(""), |file| {
            self.files.iter().any(|(path, _)| Path::new(path) == file)
        })?;
        let tgz = self.tarball()?;
        let release = IndexVersion {
            artifact: format!("{}-{}.tgz", package.id, package.version),
            integrity: integrity_of(&tgz),
            version: package.version,
            anya: package.anya.compat.unwrap_or_default(),
            protocol: package.anya.protocol.unwrap_or(PLUGIN_PROTOCOL),
            provides: self.provides.to_vec(),
            host: package.anya.host.is_some(),
            streaming: false,
            permissions: package.anya.permissions,
        };
        let entry = IndexPlugin {
            id: package.id,
            name: package.name,
            description: package.description,
            versions: vec![release],
        };
        Ok((entry, tgz))
    }

    /// The files under `package/`, as `npm pack` lays them out, with fixed
    /// times so the same files always give the same integrity hash.
    fn tarball(&self) -> Result<Vec<u8>, PluginError> {
        let failed = |e: std::io::Error| PluginError::Archive {
            what: "a built-in plugin".to_owned(),
            reason: e.to_string(),
        };
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        for (path, text) in self.files {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            tar.append_data(&mut header, format!("package/{path}"), text.as_bytes())
                .map_err(failed)?;
        }
        let mut gz = tar.into_inner().map_err(failed)?;
        gz.flush().map_err(failed)?;
        gz.finish().map_err(failed)
    }
}
