//! The built-in pieces as plugins on `anya-plugin-rt`
//! (docs/decisions/0012-built-in-plugin-runtime.md): which engines Anya ships,
//! what plugins serve into, and which of them the config wants running.

use std::sync::{Arc, LazyLock};

use anya_config::{Config, EngineRole, Recognition};
use anya_core::engine::Engine;
use anya_core::hooks::{ActionHook, ContextHook, TransformHook};
use anya_i18n::Catalog;
use anya_plugin_rt::{Active, ApplyError, Composed, Plugin, Point, Runtime, compose};

use crate::community::Community;

/// The id of the built-in plugin that serves every community plugin's hooks.
pub const COMMUNITY_HOOKS: &str = "community-hooks";

/// What plugins serve into. Cloning gives another handle on the same points.
#[derive(Clone, Default)]
pub struct Points {
    /// The engines Anya can open a session on: built-in ones the user has not
    /// turned off, and the engines of community plugins the host has loaded.
    pub engines: Point<dyn Engine>,
    /// The context hooks a dictation asks before it opens: built-in ones and
    /// those of community plugins.
    pub contexts: Point<dyn ContextHook>,
    /// The transforms of a dictation's final text.
    pub transforms: Point<dyn TransformHook>,
    /// The actions run on a dictation's final text.
    pub actions: Point<dyn ActionHook>,
}

/// The models and the whisper-server Anya runs for the whisper.cpp engine
/// (docs/decisions/0018), kept under the data directory.
static LOCAL: LazyLock<Arc<anya_whisper::Local>> = LazyLock::new(|| {
    // Without a home directory the config cannot load either, so Anya
    // never runs a session on this fallback.
    let data = anya_config::data_dir().unwrap_or_else(|_| std::env::temp_dir().join("anya"));
    anya_whisper::Local::new(data.join("models"))
});

/// The whisper.cpp engine, wrapped with the server Anya can run for it.
static WHISPER: LazyLock<Arc<dyn Engine>> =
    LazyLock::new(|| LOCAL.wrap(anya_engine_whisper_cpp::engine()));

/// The server Anya runs for the whisper.cpp engine, and its models.
pub fn local() -> &'static Arc<anya_whisper::Local> {
    &LOCAL
}

/// The id of the engine [`local`] runs a server for.
pub fn local_engine() -> &'static str {
    &WHISPER.manifest().id
}

/// The engines Anya ships, one crate each.
pub fn engines() -> Vec<Arc<dyn Engine>> {
    static ENGINES: LazyLock<Vec<Arc<dyn Engine>>> = LazyLock::new(|| {
        vec![
            anya_engine_doubao::engine(),
            anya_engine_dashscope::engine(),
            anya_engine_openai::engine(),
            Arc::clone(&WHISPER),
            anya_engine_auralwise::engine(),
            anya_engine_mimo::engine(),
        ]
    });
    ENGINES.clone()
}

/// The logo of built-in engine `id`, from its crate; `None` for an engine
/// whose crate has none, or an id that is not built in.
pub fn engine_icon(id: &str) -> Option<&'static [u8]> {
    static ICONS: LazyLock<Vec<(String, &'static [u8])>> = LazyLock::new(|| {
        [
            (anya_engine_doubao::engine(), anya_engine_doubao::ICON),
            (anya_engine_dashscope::engine(), anya_engine_dashscope::ICON),
            (anya_engine_openai::engine(), anya_engine_openai::ICON),
            (
                anya_engine_whisper_cpp::engine(),
                anya_engine_whisper_cpp::ICON,
            ),
            (anya_engine_mimo::engine(), anya_engine_mimo::ICON),
        ]
        .into_iter()
        .map(|(engine, icon)| (engine.manifest().id.clone(), icon))
        .collect()
    });
    ICONS.iter().find(|(e, _)| e == id).map(|(_, icon)| *icon)
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

/// Every built-in plugin: the engines, then the one that serves the hooks
/// of community plugins, which `community` holds.
pub fn plugins(community: Arc<Community>) -> Vec<Box<dyn Plugin<Points>>> {
    let mut all: Vec<Box<dyn Plugin<Points>>> = engines()
        .into_iter()
        .map(|engine| Box::new(EnginePlugin(engine)) as Box<dyn Plugin<Points>>)
        .collect();
    all.push(Box::new(CommunityPlugin(community)));
    all
}

/// The ids of the built-in plugins, engines first.
pub fn ids() -> Vec<String> {
    engines()
        .iter()
        .map(|engine| engine.manifest().id.clone())
        .chain([COMMUNITY_HOOKS.to_owned()])
        .collect()
}

/// The plugin that serves every community plugin's hooks: switched off, none
/// of them run.
struct CommunityPlugin(Arc<Community>);

impl Plugin<Points> for CommunityPlugin {
    fn id(&self) -> &str {
        COMMUNITY_HOOKS
    }

    fn apply(&self, points: &Points) -> Result<Active, ApplyError> {
        Ok(self.0.serve(points))
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anya_core::hooks::{Call, HookFuture, Runs};

    use super::*;
    use crate::community::Hooks;

    #[test]
    fn each_built_in_engine_logo_is_an_icon_anya_shows() {
        let mut with_logo = 0;
        for engine in engines() {
            let id = &engine.manifest().id;
            if let Some(icon) = engine_icon(id) {
                anya_plugin_host::check_icon(id, icon)
                    .unwrap_or_else(|e| panic!("{id}'s logo: {e}"));
                with_logo += 1;
            }
        }
        // AuralWise publishes no logo file.
        assert_eq!(with_logo, engines().len() - 1);
        assert_eq!(engine_icon("no-such-engine"), None);
    }

    struct Fixed(&'static str);

    impl TransformHook for Fixed {
        fn plugin(&self) -> &str {
            self.0
        }
        fn runs(&self) -> Runs {
            Runs::Host
        }
        fn transform<'a>(
            &'a self,
            text: &'a str,
            _call: &'a Call,
            _budget: Duration,
        ) -> HookFuture<'a, String> {
            Box::pin(async move { Ok(format!("{text}{}", self.0)) })
        }
    }

    #[test]
    fn community_hooks_are_served_while_the_plugin_is_on_and_follow_the_host() {
        let community = Community::new();
        let points = Points::default();
        let mut rt = Runtime::new(points.clone(), plugins(community.clone())).expect("rt");
        let hooks = |ids: &[&'static str]| Hooks {
            transforms: ids
                .iter()
                .map(|id| Arc::new(Fixed(id)) as Arc<dyn TransformHook>)
                .collect(),
            ..Hooks::default()
        };
        let served = || -> Vec<String> {
            points
                .transforms
                .all()
                .iter()
                .map(|h| h.plugin().to_owned())
                .collect()
        };

        community.set_host(hooks(&["early"]));
        rt.reconcile(|_| false);
        assert!(served().is_empty(), "community-hooks is off");

        rt.reconcile(|id| id == COMMUNITY_HOOKS);
        assert_eq!(served(), ["early"]);

        community.set_host(hooks(&["early", "late"]));
        assert_eq!(
            served(),
            ["early", "late"],
            "a host change reaches the point"
        );

        rt.reconcile(|_| false);
        assert!(served().is_empty(), "off again: none of them run");
        community.set_host(hooks(&["other"]));
        assert!(served().is_empty(), "an off plugin is not served into");
    }
}
