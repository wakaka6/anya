//! Installed plugins: unpacked packages and `plugins.json`, the record of
//! which version is pinned and whether the user enabled it.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anya_protocol::{Index, PLUGIN_PROTOCOL, PluginKind};
use serde::{Deserialize, Serialize};

use crate::PluginError;
use crate::registry::{
    MAX_ARTIFACT_BYTES, download, find_release, unpack_tarball, verify_integrity,
};

/// Where plugins and the runtime that runs them live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDirs {
    /// Holds `plugins.json` and one directory per installed plugin id, each
    /// with one directory per installed version.
    pub plugins: PathBuf,
    /// Holds the downloaded Bun and the host's script files.
    pub runtime: PathBuf,
}

impl PluginDirs {
    /// The usual layout under one data directory: `plugins/` and `runtime/`.
    pub fn under(data: &Path) -> PluginDirs {
        PluginDirs {
            plugins: data.join("plugins"),
            runtime: data.join("runtime"),
        }
    }
}

/// One installed plugin, as `plugins.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// The plugin id.
    pub id: String,
    /// The version that is pinned.
    pub version: String,
    /// The integrity hash of the tarball the version was installed from.
    pub integrity: String,
    /// The user turned the plugin on; only enabled plugins load and receive audio.
    pub enabled: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    installed: Vec<Installed>,
}

/// The installed plugins. Cheap to clone; clones share one lock, so the
/// record is never written by two operations at once. A second lock, on a
/// file beside the record, keeps other Anya processes (`anya plugin` while
/// the daemon runs) out of the same read-modify-write.
#[derive(Clone)]
pub struct Store {
    dirs: PluginDirs,
    lock: Arc<Mutex<()>>,
}

/// Held for the length of one change to `plugins.json`; both locks go with it.
struct Held<'a> {
    _process: MutexGuard<'a, ()>,
    _file: File,
}

impl Store {
    /// A store over `dirs`. Nothing is created until the first install.
    pub fn new(dirs: PluginDirs) -> Store {
        Store {
            dirs,
            lock: Arc::default(),
        }
    }

    /// Where the packages live.
    pub fn dirs(&self) -> &PluginDirs {
        &self.dirs
    }

    fn record_path(&self) -> PathBuf {
        self.dirs.plugins.join("plugins.json")
    }

    /// Takes both locks for a change to the record.
    fn guard(&self) -> Result<Held<'_>, PluginError> {
        let process = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::create_dir_all(&self.dirs.plugins)
            .map_err(|e| PluginError::io(&self.dirs.plugins, &e))?;
        let path = self.record_path().with_extension("json.lock");
        let file = File::options()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| PluginError::io(&path, &e))?;
        file.lock().map_err(|e| PluginError::io(&path, &e))?;
        Ok(Held {
            _process: process,
            _file: file,
        })
    }

    /// The record on disk. A missing file is an empty record; a file that
    /// cannot be read or parsed is an error, never an empty record, so a
    /// change cannot overwrite the list of what is installed.
    fn read(&self) -> Result<Record, PluginError> {
        let path = self.record_path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| PluginError::Io {
                path: path.display().to_string(),
                reason: format!("it is not a valid plugin record ({e}); fix or remove it"),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
            Err(e) => Err(PluginError::io(&path, &e)),
        }
    }

    fn write(&self, record: &Record) -> Result<(), PluginError> {
        let path = self.record_path();
        std::fs::create_dir_all(&self.dirs.plugins)
            .map_err(|e| PluginError::io(&self.dirs.plugins, &e))?;
        let json = serde_json::to_vec_pretty(record).map_err(|e| PluginError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        if path.exists() {
            let backup = path.with_extension("json.bak");
            std::fs::copy(&path, &backup).map_err(|e| PluginError::io(&backup, &e))?;
        }
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, json).map_err(|e| PluginError::io(&partial, &e))?;
        std::fs::rename(&partial, &path).map_err(|e| PluginError::io(&path, &e))
    }

    /// Every installed plugin, in install order.
    pub fn installed(&self) -> Result<Vec<Installed>, PluginError> {
        Ok(self.read()?.installed)
    }

    /// Where `plugin`'s package is unpacked.
    pub fn package_dir(&self, plugin: &Installed) -> PathBuf {
        self.dirs.plugins.join(&plugin.id).join(&plugin.version)
    }

    /// The enabled plugins and their package directories, for the host to load.
    pub fn enabled(&self) -> Result<Vec<(Installed, PathBuf)>, PluginError> {
        Ok(self
            .installed()?
            .into_iter()
            .filter(|p| p.enabled)
            .map(|p| {
                let dir = self.package_dir(&p);
                (p, dir)
            })
            .collect())
    }

    /// Installs `id` at `version` from `index`: checks that the release is an
    /// engine for this plugin protocol and for `anya_version`, downloads its
    /// tarball, checks its integrity hash and unpacks it. The plugin is
    /// recorded disabled; enabling is the user's next step. Installing over an
    /// installed plugin replaces its version, and also leaves it disabled.
    pub async fn install(
        &self,
        index: &Index,
        id: &str,
        version: &str,
        anya_version: &str,
    ) -> Result<Installed, PluginError> {
        check_id(id)?;
        let parsed = semver::Version::parse(version).map_err(|_| PluginError::Invalid {
            what: "version",
            value: version.to_owned(),
        })?;
        let release = find_release(index, id, version)?;
        let incompatible = |reason: String| PluginError::Incompatible {
            id: id.to_owned(),
            version: version.to_owned(),
            reason,
        };
        if release.kind != PluginKind::Engine {
            return Err(incompatible("it is not an engine plugin".to_owned()));
        }
        if release.protocol != PLUGIN_PROTOCOL {
            return Err(incompatible(format!(
                "it was built for plugin protocol {}, Anya speaks {PLUGIN_PROTOCOL}",
                release.protocol
            )));
        }
        let range = semver::VersionReq::parse(&release.anya).map_err(|e| {
            incompatible(format!("its Anya range {:?} is invalid: {e}", release.anya))
        })?;
        let anya = semver::Version::parse(anya_version).map_err(|_| PluginError::Invalid {
            what: "Anya version",
            value: anya_version.to_owned(),
        })?;
        if !range.matches(&anya) {
            return Err(incompatible(format!(
                "it needs Anya {}, this is {anya_version}",
                release.anya
            )));
        }

        let bytes = download(&release.artifact, MAX_ARTIFACT_BYTES).await?;
        verify_integrity(&release.artifact, &bytes, &release.integrity)?;

        let store = self.clone();
        let (id, integrity, artifact) = (
            id.to_owned(),
            release.integrity.clone(),
            release.artifact.clone(),
        );
        let version = parsed.to_string();
        tokio::task::spawn_blocking(move || {
            store.place(&id, &version, &integrity, &artifact, &bytes)
        })
        .await
        .map_err(|e| PluginError::Io {
            path: "install".to_owned(),
            reason: e.to_string(),
        })?
    }

    /// Unpacks `tgz` as `id`/`version` and records it.
    fn place(
        &self,
        id: &str,
        version: &str,
        integrity: &str,
        artifact: &str,
        tgz: &[u8],
    ) -> Result<Installed, PluginError> {
        let _guard = self.guard()?;
        let mut record = self.read()?;
        let base = self.dirs.plugins.join(id);
        let partial = base.join(format!(".{version}.partial"));
        let target = base.join(version);
        remove_dir(&partial)?;
        std::fs::create_dir_all(&partial).map_err(|e| PluginError::io(&partial, &e))?;
        if let Err(e) = unpack_tarball(artifact, tgz, &partial) {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(e);
        }
        remove_dir(&target)?;
        std::fs::rename(&partial, &target).map_err(|e| PluginError::io(&target, &e))?;

        let installed = Installed {
            id: id.to_owned(),
            version: version.to_owned(),
            integrity: integrity.to_owned(),
            enabled: false,
        };
        let replaced = match record.installed.iter_mut().find(|p| p.id == id) {
            Some(entry) => Some(std::mem::replace(entry, installed.clone())),
            None => {
                record.installed.push(installed.clone());
                None
            }
        };
        self.write(&record)?;
        if let Some(old) = replaced.filter(|old| old.version != version) {
            remove_dir(&base.join(&old.version))?;
        }
        Ok(installed)
    }

    /// Turns a plugin on; the host loads it the next time it starts.
    pub fn enable(&self, id: &str) -> Result<(), PluginError> {
        self.set_enabled(id, true)
    }

    /// Turns a plugin off; it stays installed.
    pub fn disable(&self, id: &str) -> Result<(), PluginError> {
        self.set_enabled(id, false)
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), PluginError> {
        let _guard = self.guard()?;
        let mut record = self.read()?;
        let entry = record
            .installed
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| PluginError::NotInstalled(id.to_owned()))?;
        entry.enabled = enabled;
        self.write(&record)
    }

    /// Deletes a plugin and its files.
    pub fn remove(&self, id: &str) -> Result<(), PluginError> {
        check_id(id)?;
        let _guard = self.guard()?;
        let mut record = self.read()?;
        let before = record.installed.len();
        record.installed.retain(|p| p.id != id);
        if record.installed.len() == before {
            return Err(PluginError::NotInstalled(id.to_owned()));
        }
        self.write(&record)?;
        remove_dir(&self.dirs.plugins.join(id))
    }
}

/// An id becomes a directory name, so it must not be able to leave `plugins/`.
fn check_id(id: &str) -> Result<(), PluginError> {
    let ok = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(PluginError::Invalid {
            what: "plugin id",
            value: id.to_owned(),
        })
    }
}

fn remove_dir(dir: &Path) -> Result<(), PluginError> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PluginError::io(dir, &e)),
    }
}
