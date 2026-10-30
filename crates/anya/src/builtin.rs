//! The built-in pieces as plugins on `anya-plugin-rt`
//! (docs/decisions/0012-built-in-plugin-runtime.md): which engines Anya ships,
//! what plugins serve into, and which of them the config wants running.

use std::sync::Arc;
use std::sync::LazyLock;

use anya_config::{Config, EngineRole, Recognition};
use anya_core::engine::Engine;
use anya_i18n::Catalog;
use anya_plugin_rt::{Active, ApplyError, Composed, Plugin, Point, Runtime, compose};

/// What plugins serve into. Cloning gives another handle on the same points.
#[derive(Clone, Default)]
pub struct Points {
    /// The engines Anya can open a session on: built-in ones the user has not
    /// turned off, and the engines of community plugins the host has loaded.
    pub engines: Point<dyn Engine>,
}

/// The engines Anya ships, one crate each.
pub fn engines() -> Vec<Arc<dyn Engine>> {
    static ENGINES: LazyLock<Vec<Arc<dyn Engine>>> = LazyLock::new(|| {
        vec![
            anya_engine_doubao::engine(),
            anya_engine_dashscope::engine(),
            anya_engine_openai::engine(),
            anya_engine_auralwise::engine(),
            anya_engine_mimo::engine(),
        ]
    });
    ENGINES.clone()
}

/// A built-in engine as a plugin: it serves itself at the engines point.
struct EnginePlugin(Arc<dyn Engine>);

impl Plugin<Points> for EnginePlugin {
    fn id(&self) -> &str {
        &self.0.manifest().id
    }

    fn apply(&self, points: &Points) -> Result<Active, ApplyError> {
        Ok(Active::new().hold(points.engines.serve(Arc::clone(&self.0))))
    }
}

/// Every built-in plugin, in the order engines are listed.
pub fn plugins() -> Vec<Box<dyn Plugin<Points>>> {
    engines()
        .into_iter()
        .map(|engine| Box::new(EnginePlugin(engine)) as Box<dyn Plugin<Points>>)
        .collect()
}

/// The ids of the built-in plugins.
pub fn ids() -> Vec<String> {
    plugins().iter().map(|p| p.id().to_owned()).collect()
}

/// What `cfg` wants of each built-in plugin: on unless its row says off, and
/// on while any use (dictation, continuous, remote) has chosen it.
pub fn composed(cfg: &Config, ids: &[String]) -> Vec<Composed> {
    let in_use = cfg.recognition.engines_in_use();
    compose(ids.iter().map(String::as_str), &cfg.plugins.builtin, |id| {
        in_use.contains(&id)
    })
}

/// The uses that chose engine `id`, named in the catalog's language under
/// `<prefix>.<role key>` and joined with `<prefix>.join`.
pub fn use_names(recognition: &Recognition, id: &str, c: &Catalog, prefix: &str) -> String {
    let names: Vec<String> = EngineRole::ALL
        .into_iter()
        .filter(|role| recognition.engine_id(*role) == id)
        .map(|role| c.t(&format!("{prefix}.{}", role.key())).to_owned())
        .collect();
    names.join(c.t(&format!("{prefix}.join")))
}

/// Brings the running built-in plugins to what `cfg` wants.
pub fn reconcile(runtime: &mut Runtime<Points>, cfg: &Config) {
    let ids: Vec<String> = runtime.ids().map(str::to_owned).collect();
    let wanted = composed(cfg, &ids);
    runtime.reconcile(|id| wanted.iter().any(|c| c.on && c.id == id));
}
