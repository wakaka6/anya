//! The hooks community plugins serve (docs/decisions/0016): the host's, from
//! the plugins running in Bun, and the inline ones, from the QuickJS modules
//! of the enabled packages. The built-in `community-hooks` plugin puts them at
//! the points; switched off, none are there.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anya_config::Config;
use anya_core::hooks::{ActionHook, ContextHook, TransformHook};
use anya_plugin_host::{Store, read_package};
use anya_plugin_rt::{Active, Registration};
use anya_protocol::{Limits, Provide};
use anya_script::InlinePlugin;
use tokio::sync::Mutex as AsyncMutex;

use crate::builtin::Points;

/// The hooks of some plugins.
#[derive(Clone, Default)]
pub struct Hooks {
    /// Hooks that run before a session opens.
    pub contexts: Vec<Arc<dyn ContextHook>>,
    /// Hooks that rewrite the final text.
    pub transforms: Vec<Arc<dyn TransformHook>>,
    /// Hooks that act on the final text.
    pub actions: Vec<Arc<dyn ActionHook>>,
}

/// How an enabled package's inline entry stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineState {
    /// Loaded; says which hooks it exports.
    Ready(Vec<Provide>),
    /// Not loaded; why, in English.
    Failed(String),
}

/// What `community-hooks` is serving into while it is on.
struct Serving {
    points: Points,
    held: Vec<Registration>,
}

#[derive(Default)]
struct State {
    host: Hooks,
    inline: Hooks,
    /// What the enabled packages declare, by plugin id.
    declared: BTreeMap<String, Declared>,
    inline_state: BTreeMap<String, InlineState>,
    serving: Option<Serving>,
}

impl State {
    /// Serves what is held now in place of what was served.
    fn republish(&mut self) {
        let Some(serving) = &mut self.serving else {
            return;
        };
        serving.held.clear();
        let points = &serving.points;
        for hook in self.host.contexts.iter().chain(&self.inline.contexts) {
            serving.held.push(points.contexts.serve(Arc::clone(hook)));
        }
        for hook in self.host.transforms.iter().chain(&self.inline.transforms) {
            serving.held.push(points.transforms.serve(Arc::clone(hook)));
        }
        for hook in self.host.actions.iter().chain(&self.inline.actions) {
            serving.held.push(points.actions.serve(Arc::clone(hook)));
        }
    }
}

/// An inline plugin as it was loaded: what decides that it must load again.
#[derive(Clone)]
struct Loaded {
    id: String,
    version: String,
    memory: usize,
    plugin: Result<InlinePlugin, String>,
}

/// What an enabled package declares that running its hooks needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declared {
    /// The budgets it asks for.
    pub limits: Limits,
}

/// The community plugins' hooks and the inline modules behind some of them.
/// Shared by the `community-hooks` plugin and the app.
pub struct Community {
    state: Mutex<State>,
    loaded: AsyncMutex<Vec<Loaded>>,
}

impl Community {
    /// Nothing loaded, nothing served.
    pub fn new() -> Arc<Community> {
        Arc::new(Community {
            state: Mutex::new(State::default()),
            loaded: AsyncMutex::new(Vec::new()),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-update of plain lists; they stay usable.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts serving every hook held into `points`, and keeps them current,
    /// until the returned [`Active`] is dropped.
    pub fn serve(self: &Arc<Community>, points: &Points) -> Active {
        let mut state = self.state();
        state.serving = Some(Serving {
            points: points.clone(),
            held: Vec::new(),
        });
        state.republish();
        drop(state);
        Active::new().hold(Withdraw(Arc::clone(self)))
    }

    /// The hooks of the plugins running in Bun, replacing the last ones.
    pub fn set_host(&self, host: Hooks) {
        let mut state = self.state();
        state.host = host;
        state.republish();
    }

    /// What the enabled packages declare, by plugin id.
    pub fn declared(&self) -> BTreeMap<String, Declared> {
        self.state().declared.clone()
    }

    /// How plugin `id`'s inline entry stands; `None` when it has none or
    /// has not been looked at yet.
    pub fn inline_state(&self, id: &str) -> Option<InlineState> {
        self.state().inline_state.get(id).cloned()
    }

    /// Brings the inline plugins to the enabled packages: loads those that
    /// are new or whose version or memory budget changed, drops those no
    /// longer enabled, and serves the result. One refresh at a time; each
    /// reads the config when it starts.
    pub async fn refresh(&self, store: &Store, config: &Mutex<Config>) {
        let mut loaded = self.loaded.lock().await;
        let plugins = config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .plugins
            .clone();
        let enabled = match store.enabled() {
            Ok(enabled) => enabled,
            Err(e) => {
                tracing::warn!("cannot list the enabled plugins: {e}");
                return;
            }
        };
        let previous = std::mem::take(&mut *loaded);
        let mut declared = BTreeMap::new();
        let mut states = BTreeMap::new();
        let mut hooks = Hooks::default();
        for (installed, dir) in enabled {
            let package = match read_package(&dir) {
                Ok(package) => package,
                Err(e) => {
                    states.insert(installed.id, InlineState::Failed(e.to_string()));
                    continue;
                }
            };
            declared.insert(
                installed.id.clone(),
                Declared {
                    limits: package.anya.limits,
                },
            );
            let Some(file) = package.anya.inline.as_deref() else {
                continue;
            };
            let budgets = plugins.budgets(&installed.id, package.anya.limits);
            let entry = match previous.iter().find(|l| {
                l.id == installed.id
                    && l.version == installed.version
                    && l.memory == budgets.inline_memory
            }) {
                Some(entry) => entry.clone(),
                None => Loaded {
                    plugin: match inside(&package.dir, file) {
                        Some(path) => InlinePlugin::load(&installed.id, &path, &budgets)
                            .await
                            .map_err(|e| e.to_string()),
                        None => Err("the inline entry is outside the package".to_owned()),
                    },
                    id: installed.id.clone(),
                    version: installed.version.clone(),
                    memory: budgets.inline_memory,
                },
            };
            match &entry.plugin {
                Ok(plugin) => {
                    states.insert(installed.id, InlineState::Ready(plugin.provides()));
                    hooks.contexts.extend(plugin.context());
                    hooks.transforms.extend(plugin.transform());
                }
                Err(e) => {
                    tracing::warn!(plugin = %installed.id, "inline plugin did not load: {e}");
                    states.insert(installed.id, InlineState::Failed(e.clone()));
                }
            }
            loaded.push(entry);
        }
        let mut state = self.state();
        state.inline = hooks;
        state.declared = declared;
        state.inline_state = states;
        state.republish();
    }
}

/// Withdraws what `community-hooks` served when the plugin stops.
struct Withdraw(Arc<Community>);

impl Drop for Withdraw {
    fn drop(&mut self) {
        self.0.state().serving = None;
    }
}

/// `file` inside the package at `dir`; `None` when it leaves it.
fn inside(dir: &Path, file: &str) -> Option<PathBuf> {
    let relative = Path::new(file);
    relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        .then(|| dir.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_stays_inside_its_package() {
        let dir = Path::new("/p");
        assert_eq!(inside(dir, "a.js"), Some(PathBuf::from("/p/a.js")));
        assert_eq!(
            inside(dir, "./dist/a.js"),
            Some(PathBuf::from("/p/./dist/a.js"))
        );
        assert_eq!(inside(dir, "../a.js"), None);
        assert_eq!(inside(dir, "/etc/a.js"), None);
    }
}
