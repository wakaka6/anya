//! Plugin operations the daemon and `anya plugin` share: where the index is,
//! which release to install, and the command line's output. The daemon's
//! side (loading, reloading) lives in `main.rs`.

use std::io::Write;

use anya_config::Config;
use anya_i18n::Catalog;
use anya_plugin_host::{
    BunPath, DEFAULT_INDEX_URL, Installed, PluginDirs, PluginError, Registry, Store,
};
use anya_protocol::{Index, IndexPlugin, IndexVersion};
use anya_runtime::PluginIssue;

use crate::builtin;

/// What `anya plugin` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// List the registry's plugins, those matching `query` when given.
    Search(Option<String>),
    /// Install a release, disabled; the newest one without `version`.
    Install {
        /// The plugin id.
        id: String,
        /// `--version` or the part after `@`.
        version: Option<String>,
    },
    /// Turn an installed plugin on.
    Enable(String),
    /// Turn an installed plugin off.
    Disable(String),
    /// Delete an installed plugin.
    Remove(String),
    /// List installed plugins.
    List,
    /// List the built-in plugins, whether each is on and why.
    BuiltinList,
    /// Turn a built-in plugin on or off.
    BuiltinSet {
        /// The built-in plugin id.
        id: String,
        /// The wanted state.
        on: bool,
    },
}

/// `error` as the code the UIs translate; the English text stays in logs.
pub fn issue(error: &PluginError) -> PluginIssue {
    match error {
        PluginError::Fetch { url, reason } => PluginIssue::Fetch {
            url: url.clone(),
            reason: reason.clone(),
        },
        PluginError::Index(reason) => PluginIssue::Index {
            reason: reason.clone(),
        },
        PluginError::IndexVersion { found, supported } => PluginIssue::IndexVersion {
            found: *found,
            supported: *supported,
        },
        PluginError::NotInIndex { id, version } => PluginIssue::NotInIndex {
            id: id.clone(),
            version: version.clone(),
        },
        PluginError::Incompatible {
            id,
            version,
            reason,
        } => PluginIssue::Incompatible {
            id: id.clone(),
            version: version.clone(),
            reason: reason.clone(),
        },
        PluginError::Integrity { what, .. } => PluginIssue::Integrity { what: what.clone() },
        PluginError::IntegrityFormat(value) => PluginIssue::IntegrityFormat {
            value: value.clone(),
        },
        PluginError::Invalid { value, .. } => PluginIssue::Invalid {
            value: value.clone(),
        },
        PluginError::NotInstalled(id) => PluginIssue::NotInstalled { id: id.clone() },
        PluginError::Archive { what, reason } => PluginIssue::Archive {
            what: what.clone(),
            reason: reason.clone(),
        },
        PluginError::Io { path, reason } => PluginIssue::Io {
            path: path.clone(),
            reason: reason.clone(),
        },
        PluginError::Bun(reason) => PluginIssue::Bun {
            reason: reason.clone(),
        },
    }
}

/// The oldest Bun plugins run on, as a missing-Bun issue.
pub fn bun_missing() -> PluginIssue {
    PluginIssue::BunMissing {
        version: anya_plugin_host::MIN_BUN_VERSION.to_owned(),
    }
}

/// The registry index to read: the config's, else the anya-plugins one.
pub fn index_url(config: &Config) -> String {
    config
        .plugins
        .index()
        .unwrap_or(DEFAULT_INDEX_URL)
        .to_owned()
}

/// The newest release of `id` in `index`; the index lists oldest first.
pub fn latest<'a>(index: &'a Index, id: &str) -> Option<&'a IndexVersion> {
    index
        .plugins
        .iter()
        .find(|p| p.id == id)
        .and_then(|p| p.versions.last())
}

/// Installs `id` (at `version`, else its newest release) from the index at
/// `url`, disabled. Provisions Bun first when none is usable; `bun_download`
/// is called just before a download starts.
pub async fn install(
    dirs: &PluginDirs,
    url: &str,
    id: &str,
    version: Option<&str>,
    bun_download: impl FnOnce(),
) -> Result<(Installed, IndexVersion), PluginError> {
    let index = Registry::fetch(url).await?;
    let release = match version {
        Some(v) => anya_plugin_host::find_release(&index, id, v)?,
        None => latest(&index, id).ok_or_else(|| PluginError::NotInIndex {
            id: id.to_owned(),
            version: "latest".to_owned(),
        })?,
    }
    .clone();
    if BunPath::detect(&dirs.runtime).await.is_none() {
        bun_download();
        BunPath::provision(&dirs.runtime).await?;
    }
    let installed = Store::new(dirs.clone())
        .install(&index, id, &release.version, env!("CARGO_PKG_VERSION"))
        .await?;
    Ok((installed, release))
}

fn matches(plugin: &IndexPlugin, query: &str, locale: &str) -> bool {
    let query = query.to_lowercase();
    plugin.id.to_lowercase().contains(&query)
        || plugin.name.get(locale).to_lowercase().contains(&query)
        || plugin
            .description
            .get(locale)
            .to_lowercase()
            .contains(&query)
}

fn permissions(release: &IndexVersion, c: &Catalog) -> String {
    if release.permissions.is_empty() {
        c.t("cli.plugin.permissions_none").to_owned()
    } else {
        release.permissions.join(", ")
    }
}

fn mode(release: &IndexVersion, c: &Catalog) -> String {
    c.t(if release.streaming {
        "cli.plugin.streaming"
    } else {
        "cli.plugin.batch"
    })
    .to_owned()
}

/// Runs `action` against the plugin store under `data`, printing to `out` in
/// `catalog`'s language. The daemon is not signalled: what changes applies
/// the next time Anya starts, and the output says so.
pub async fn run(
    action: Action,
    config: &Config,
    config_path: &std::path::Path,
    data: &std::path::Path,
    catalog: &Catalog,
    out: &mut impl Write,
) -> Result<(), String> {
    let c = catalog;
    let dirs = PluginDirs::under(data);
    let store = Store::new(dirs.clone());
    let failed = |e: PluginError| {
        let (key, args) = issue(&e).message();
        let args: Vec<(&str, &str)> = args.iter().map(|(n, v)| (*n, v.as_str())).collect();
        c.tf(key, &args)
    };
    let io = |e: std::io::Error| e.to_string();
    match action {
        Action::Search(query) => {
            let index = Registry::fetch(&index_url(config)).await.map_err(failed)?;
            let mut shown = 0;
            for plugin in &index.plugins {
                let Some(release) = plugin.versions.last() else {
                    continue;
                };
                if query
                    .as_deref()
                    .is_some_and(|q| !matches(plugin, q, c.locale()))
                {
                    continue;
                }
                shown += 1;
                let name = match plugin.name.get(c.locale()) {
                    "" => plugin.id.as_str(),
                    n => n,
                };
                writeln!(
                    out,
                    "{}",
                    c.tf(
                        "cli.plugin.search.line",
                        &[
                            ("id", &plugin.id),
                            ("version", &release.version),
                            ("name", name),
                            ("mode", &mode(release, c)),
                            ("permissions", &permissions(release, c)),
                        ],
                    )
                )
                .map_err(io)?;
                let description = plugin.description.get(c.locale());
                if !description.is_empty() {
                    writeln!(out, "    {description}").map_err(io)?;
                }
            }
            if shown == 0 {
                writeln!(out, "{}", c.t("cli.plugin.search.none")).map_err(io)?;
            }
        }
        Action::Install { id, version } => {
            let (installed, release) =
                install(&dirs, &index_url(config), &id, version.as_deref(), || {
                    let _ = writeln!(std::io::stderr(), "{}", c.t("cli.plugin.bun_download"));
                })
                .await
                .map_err(failed)?;
            writeln!(
                out,
                "{}",
                c.tf(
                    "cli.plugin.installed",
                    &[("id", &installed.id), ("version", &installed.version)]
                )
            )
            .map_err(io)?;
            writeln!(
                out,
                "{}",
                c.tf(
                    "cli.plugin.declared",
                    &[("permissions", &permissions(&release, c))]
                )
            )
            .map_err(io)?;
            writeln!(out, "{}", c.tf("cli.plugin.audio_warning", &[("id", &id)])).map_err(io)?;
        }
        Action::Enable(id) => {
            store.enable(&id).map_err(failed)?;
            writeln!(out, "{}", c.tf("cli.plugin.enabled", &[("id", &id)])).map_err(io)?;
        }
        Action::Disable(id) => {
            store.disable(&id).map_err(failed)?;
            writeln!(out, "{}", c.tf("cli.plugin.disabled", &[("id", &id)])).map_err(io)?;
        }
        Action::Remove(id) => {
            store.remove(&id).map_err(failed)?;
            writeln!(out, "{}", c.tf("cli.plugin.removed", &[("id", &id)])).map_err(io)?;
        }
        Action::BuiltinList => {
            for p in builtin::composed(config, &builtin::ids()) {
                let why = c.t(match (p.on, p.why) {
                    (true, anya_plugin_rt::Why::InUse) => "cli.plugin.builtin.in_use",
                    (true, _) => "cli.plugin.builtin.on",
                    (false, _) => "cli.plugin.builtin.off",
                });
                writeln!(
                    out,
                    "{}",
                    c.tf("cli.plugin.builtin.line", &[("id", &p.id), ("state", why)])
                )
                .map_err(io)?;
            }
        }
        Action::BuiltinSet { id, on } => {
            if !builtin::ids().contains(&id) {
                return Err(c.tf("cli.plugin.builtin.unknown", &[("id", &id)]));
            }
            if !on && config.recognition.engines_in_use().contains(&id.as_str()) {
                let uses =
                    builtin::use_names(&config.recognition, &id, c, "cli.plugin.builtin.use");
                return Err(c.tf("cli.plugin.builtin.chosen", &[("id", &id), ("uses", &uses)]));
            }
            let mut next = config.clone();
            next.plugins.builtin.insert(id.clone(), on);
            anya_config::save(&next, config_path).map_err(|e| e.to_string())?;
            let key = if on {
                "cli.plugin.builtin.turned_on"
            } else {
                "cli.plugin.builtin.turned_off"
            };
            writeln!(out, "{}", c.tf(key, &[("id", &id)])).map_err(io)?;
        }
        Action::List => {
            let installed = store.installed().map_err(failed)?;
            if installed.is_empty() {
                writeln!(out, "{}", c.t("cli.plugin.list.none")).map_err(io)?;
            }
            for plugin in &installed {
                let state = c.t(if plugin.enabled {
                    "cli.plugin.list.enabled"
                } else {
                    "cli.plugin.list.disabled"
                });
                writeln!(
                    out,
                    "{}",
                    c.tf(
                        "cli.plugin.list.line",
                        &[
                            ("id", &plugin.id),
                            ("version", &plugin.version),
                            ("state", state),
                        ],
                    )
                )
                .map_err(io)?;
            }
            if installed.iter().any(|p| p.enabled) && BunPath::detect(&dirs.runtime).await.is_none()
            {
                writeln!(out, "{}", c.t("cli.plugin.list.no_bun")).map_err(io)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(json: &str) -> Index {
        serde_json::from_str(json).expect("an index")
    }

    #[test]
    fn the_newest_release_is_the_last_one_listed() {
        let i = index(
            r#"{"version":1,"plugins":[{"id":"a","name":"A","versions":[
            {"version":"1.0.0","artifact":"x","integrity":"y","anya":"*","protocol":1,"kind":"engine"},
            {"version":"1.1.0","artifact":"x","integrity":"y","anya":"*","protocol":1,"kind":"engine"}]}]}"#,
        );
        assert_eq!(latest(&i, "a").map(|v| v.version.as_str()), Some("1.1.0"));
        assert!(latest(&i, "b").is_none());
    }

    #[tokio::test]
    async fn a_plugin_error_reads_in_the_users_language() {
        let data = std::env::temp_dir().join(format!("anya-plugins-{}", std::process::id()));
        let mut out = Vec::new();
        let error = run(
            Action::Enable("nope".to_owned()),
            &Config::default(),
            std::path::Path::new("/unused/config.toml"),
            &data,
            &Catalog::new("zh-CN"),
            &mut out,
        )
        .await
        .expect_err("nothing is installed");
        assert_eq!(error, "插件 nope 未安装");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[tokio::test]
    async fn a_built_in_plugin_is_turned_off_in_the_config_unless_any_use_has_chosen_it() {
        let dir = std::env::temp_dir().join(format!("anya-builtin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("config.toml");
        let ids = builtin::ids();
        assert!(ids.len() >= 4, "three uses and a spare engine");
        let mut config = Config::default();
        for (role, id) in anya_config::EngineRole::ALL.into_iter().zip(&ids) {
            config.recognition.set_engine(role, id);
        }
        let other = ids[3].clone();
        let c = Catalog::new("en");
        let mut out = Vec::new();
        let set = |id: &str, on| Action::BuiltinSet {
            id: id.to_owned(),
            on,
        };

        for (id, name) in ids.iter().zip(["dictation", "continuous", "remote"]) {
            let refused = run(set(id, false), &config, &path, &dir, &c, &mut out)
                .await
                .expect_err("an engine in use stays on");
            assert!(refused.contains(name), "names the use: {refused}");
        }
        assert!(
            run(set("nope", false), &config, &path, &dir, &c, &mut out)
                .await
                .is_err()
        );
        assert!(!path.exists(), "refusals write nothing");

        run(set(&other, false), &config, &path, &dir, &c, &mut out)
            .await
            .expect("off");
        let saved = anya_config::load(&path).expect("load");
        assert_eq!(saved.plugins.builtin.get(&other), Some(&false));
        let wanted = builtin::composed(&saved, &ids);
        assert!(wanted.iter().any(|p| p.id == other && !p.on));
        for id in &ids[..3] {
            assert!(wanted.iter().any(|p| p.id == *id && p.on));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_config_names_the_index_or_the_registry_answers() {
        let mut config = Config::default();
        assert_eq!(index_url(&config), DEFAULT_INDEX_URL);
        config.plugins.index = "file:///i.json".to_owned();
        assert_eq!(index_url(&config), "file:///i.json");
    }
}
