//! Engine and plugin icons as the settings ask for them: by a name made from
//! the picture, kept on disk. A built-in engine's and an official plugin's
//! come from the binary, an installed plugin's from its package, a registry
//! plugin's is downloaded the first time it is asked for, checked against the
//! index's hash.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anya_plugin_host::{Bundled, Icons as Kept, Package, package_icon};
use anya_protocol::IndexVersion;
use anya_runtime::Icon;

/// How long a registry icon that failed to download is not asked for again.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

/// The icons.
pub struct Icons {
    icons: Kept,
    /// Each package's icon name, by package directory; a package is read once.
    packages: Mutex<HashMap<PathBuf, Option<String>>>,
    /// Registry icons by name: their URL and integrity.
    remote: Mutex<HashMap<String, (String, String)>>,
    /// When a registry icon last failed to download, by name.
    failed: Mutex<HashMap<String, Instant>>,
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Icons {
    /// The icons kept in `dir`.
    pub fn new(dir: PathBuf) -> Icons {
        Icons {
            icons: Kept::new(dir),
            packages: Mutex::default(),
            remote: Mutex::default(),
            failed: Mutex::default(),
        }
    }

    /// The name of the icon `package` gives; `None` when it gives none Anya
    /// can show.
    pub fn of_package(&self, package: &Package) -> Option<String> {
        if let Some(name) = lock(&self.packages).get(&package.dir) {
            return name.clone();
        }
        let name = match package_icon(package)
            .and_then(|icon| icon.map(|b| self.icons.keep(&b)).transpose())
        {
            Ok(name) => name,
            Err(e) => {
                tracing::debug!(plugin = %package.id, "no icon: {e}");
                None
            }
        };
        lock(&self.packages).insert(package.dir.clone(), name.clone());
        name
    }

    /// The name of the icon a plugin built into Anya gives.
    pub fn of_bundled(&self, bundled: &Bundled) -> Option<String> {
        match bundled
            .icon()
            .and_then(|icon| icon.map(|b| self.icons.keep(&b)).transpose())
        {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!("a built-in plugin's icon does not read: {e}");
                None
            }
        }
    }

    /// The name of `bytes`, an icon built into Anya.
    pub fn of_bytes(&self, bytes: &[u8]) -> Option<String> {
        self.icons
            .keep(bytes)
            .inspect_err(|e| tracing::warn!("a built-in icon does not read: {e}"))
            .ok()
    }

    /// The name of a registry release's icon, which [`Icons::get`]
    /// downloads when it is first asked for.
    pub fn of_release(&self, release: &IndexVersion) -> Option<String> {
        let (url, integrity) = (release.icon.as_ref()?, release.icon_integrity.as_ref()?);
        let name = Kept::name_of(integrity)?;
        lock(&self.remote).insert(name.clone(), (url.clone(), integrity.clone()));
        Some(name)
    }

    /// The icon named `name`; a registry icon is downloaded the first time.
    pub async fn get(&self, name: &str) -> Option<Icon> {
        if let Some(icon) = self.read(name) {
            return Some(icon);
        }
        let (url, integrity) = lock(&self.remote).get(name).cloned()?;
        if lock(&self.failed)
            .get(name)
            .is_some_and(|at| at.elapsed() < RETRY_AFTER)
        {
            return None;
        }
        if let Err(e) = self.icons.fetch(&url, &integrity).await {
            tracing::debug!(%url, "plugin icon not downloaded: {e}");
            lock(&self.failed).insert(name.to_owned(), Instant::now());
            return None;
        }
        self.read(name)
    }

    fn read(&self, name: &str) -> Option<Icon> {
        let (bytes, format) = self.icons.read(name)?;
        Some(Icon {
            bytes,
            content_type: format.content_type(),
        })
    }
}
