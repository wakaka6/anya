//! The plugins the Anya project writes, built into the binary from
//! `plugins/`. The market lists them without the registry and they install
//! from here (docs/decisions/0017-official-plugins-built-in.md).

use anya_plugin_host::Bundled;
use anya_protocol::{IndexPlugin, Provide};

/// Every official plugin. A package here is marked `"private": true`, so the
/// registry's workflow never releases it.
pub const PLUGINS: &[Bundled] = &[
    Bundled {
        provides: &[Provide::Context, Provide::Transform],
        files: &[
            (
                "package.json",
                include_str!("../../../plugins/spoken-punctuation/package.json"),
            ),
            (
                "inline.js",
                include_str!("../../../plugins/spoken-punctuation/inline.js"),
            ),
        ],
    },
    Bundled {
        provides: &[Provide::Context],
        files: &[
            (
                "package.json",
                include_str!("../../../plugins/app-hotwords/package.json"),
            ),
            (
                "inline.js",
                include_str!("../../../plugins/app-hotwords/inline.js"),
            ),
        ],
    },
    Bundled {
        provides: &[Provide::Action],
        files: &[
            (
                "package.json",
                include_str!("../../../plugins/daily-notes/package.json"),
            ),
            (
                "host.js",
                include_str!("../../../plugins/daily-notes/host.js"),
            ),
        ],
    },
];

/// The official plugins as registry entries, in [`PLUGINS`] order. One whose
/// package does not read is logged and left out; the tests keep that from
/// shipping.
pub fn entries() -> Vec<IndexPlugin> {
    PLUGINS
        .iter()
        .filter_map(|bundled| {
            bundled
                .entry()
                .inspect_err(|e| tracing::error!("a built-in plugin does not read: {e}"))
                .ok()
        })
        .collect()
}

/// The official plugin `id` and its entry.
pub fn find(id: &str) -> Option<(&'static Bundled, IndexPlugin)> {
    PLUGINS.iter().find_map(|bundled| {
        let entry = bundled.entry().ok()?;
        (entry.id == id).then_some((bundled, entry))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use anya_plugin_host::{PluginDirs, Store, read_package};
    use anya_script::InlinePlugin;
    use serde_json::Value;

    use super::*;

    const NO_REGISTRY: &str = "file:///nonexistent/anya-index.json";

    fn package_json(dir: &Path) -> Value {
        let text = std::fs::read_to_string(dir.join("package.json")).expect("package.json");
        serde_json::from_str(&text).expect("json")
    }

    // What ships is what `plugins/` holds and what its entries export, and
    // it installs with the registry out of reach.
    #[tokio::test]
    async fn each_official_plugin_is_its_package_and_installs_offline() {
        let data = std::env::temp_dir().join(format!("anya-official-{}", std::process::id()));
        let dirs = PluginDirs::under(&data);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        assert_eq!(entries().len(), PLUGINS.len(), "every package reads");
        for (bundled, entry) in PLUGINS.iter().zip(entries()) {
            let source = package_json(&root.join(&entry.id));
            assert_eq!(source["private"], true, "{} is never released", entry.id);
            let mut listed: BTreeSet<&str> = source["files"]
                .as_array()
                .expect("files")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            listed.insert("package.json");
            let embedded: BTreeSet<&str> = bundled.files.iter().map(|(path, _)| *path).collect();
            assert_eq!(
                embedded, listed,
                "{} ships the files npm would pack",
                entry.id
            );

            // `plugins::install` would fetch Bun for a host entry; the store
            // step is the same without it.
            let installed = if entry.versions[0].host {
                Store::new(dirs.clone()).install_bundled(bundled, env!("CARGO_PKG_VERSION"))
            } else {
                crate::plugins::install(&dirs, NO_REGISTRY, &entry.id, None, || {})
                    .await
                    .map(|(installed, _)| installed)
            }
            .unwrap_or_else(|e| panic!("{} installs offline: {e}", entry.id));
            assert!(!installed.enabled);
            let package = read_package(&dirs.plugins.join(&entry.id).join(&installed.version))
                .expect("unpacked");
            if let Some(inline) = &package.anya.inline {
                let budgets = anya_config::Config::default()
                    .plugins
                    .budgets(&entry.id, package.anya.limits);
                let loaded = InlinePlugin::load(&entry.id, &package.dir.join(inline), &budgets)
                    .await
                    .expect("loads");
                let exported: BTreeSet<_> = loaded.provides().into_iter().collect();
                let declared: BTreeSet<_> = bundled.provides.iter().copied().collect();
                assert_eq!(exported, declared, "{} provides what it exports", entry.id);
            }
        }
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn every_package_kept_out_of_the_registry_is_built_in() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let official: Vec<String> = entries().into_iter().map(|e| e.id).collect();
        for dir in std::fs::read_dir(&root).expect("plugins/") {
            let dir = dir.expect("entry").path();
            if !dir.join("package.json").is_file() {
                continue;
            }
            let package = package_json(&dir);
            if package["private"] == true {
                let id = package["anya"]["id"].as_str().expect("anya.id");
                assert!(
                    official.iter().any(|o| o == id),
                    "{id} is private but not built in"
                );
            }
        }
    }
}
