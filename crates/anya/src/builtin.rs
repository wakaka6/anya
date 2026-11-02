//! The built-in pieces as plugins on `anya-plugin-rt`
//! (docs/decisions/0012-built-in-plugin-runtime.md): which engines Anya ships,
//! what plugins serve into, and which of them the config wants running.

use std::cmp::Reverse;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anya_config::{Config, EngineRole, Recognition, Replacement, ReplacementsFile};
use anya_core::engine::Engine;
use anya_core::hooks::{ActionHook, Call, ContextHook, HookFuture, Runs, TransformHook};
use anya_i18n::Catalog;
use anya_plugin_rt::{Active, ApplyError, Composed, Plugin, Point, Runtime, compose};

use crate::community::Community;

/// The built-in replacement list's plugin id.
pub const REPLACE: &str = "replace";
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

/// Every built-in plugin: the engines, then the pieces that serve hooks.
/// `replacements` is the replacement list's file; `community` holds the
/// hooks of community plugins.
pub fn plugins(replacements: PathBuf, community: Arc<Community>) -> Vec<Box<dyn Plugin<Points>>> {
    let mut all: Vec<Box<dyn Plugin<Points>>> = engines()
        .into_iter()
        .map(|engine| Box::new(EnginePlugin(engine)) as Box<dyn Plugin<Points>>)
        .collect();
    all.push(Box::new(ReplacePlugin(Arc::new(Replace::new(
        replacements,
    )))));
    all.push(Box::new(CommunityPlugin(community)));
    all
}

/// The ids of the built-in plugins, engines first.
pub fn ids() -> Vec<String> {
    engines()
        .iter()
        .map(|engine| engine.manifest().id.clone())
        .chain([REPLACE, COMMUNITY_HOOKS].map(str::to_owned))
        .collect()
}

/// The built-in replacement list as a transform: `spoken => written` lines
/// from `replacements.txt`, read again whenever the file changes.
struct Replace {
    file: PathBuf,
    cache: Mutex<Rules>,
}

/// The rules as last read, and the file's state then.
struct Rules {
    /// Modified time and length; `None` while there is no file.
    stamp: Option<(SystemTime, u64)>,
    /// Longest spoken side first.
    list: Arc<Vec<Replacement>>,
}

impl Replace {
    fn new(file: PathBuf) -> Replace {
        Replace {
            file,
            cache: Mutex::new(Rules {
                stamp: None,
                list: Arc::default(),
            }),
        }
    }

    /// The rules in the file now; a stat per call, a read when it changed.
    fn rules(&self) -> Arc<Vec<Replacement>> {
        let stamp = std::fs::metadata(&self.file)
            .and_then(|m| Ok((m.modified()?, m.len())))
            .ok();
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.stamp != stamp {
            let (mut list, file) = anya_config::read_replacements(&self.file);
            if let ReplacementsFile::Unreadable { path, error } = file {
                tracing::warn!(path = %path.display(), %error, "the replacement list is unreadable");
            }
            list.sort_by_key(|r| Reverse(r.spoken.len()));
            *cache = Rules {
                stamp,
                list: Arc::new(list),
            };
        }
        Arc::clone(&cache.list)
    }
}

/// `text` with every `spoken` of `rules` (longest first) replaced by its
/// `written`. One pass: what a rule writes is never matched again.
fn replace_all(rules: &[Replacement], text: &str) -> String {
    if rules.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        match rules.iter().find(|r| rest.starts_with(r.spoken.as_str())) {
            Some(rule) => {
                out.push_str(&rule.written);
                rest = &rest[rule.spoken.len()..];
            }
            None => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

impl TransformHook for Replace {
    fn plugin(&self) -> &str {
        REPLACE
    }

    fn runs(&self) -> Runs {
        Runs::Builtin
    }

    fn transform<'a>(
        &'a self,
        text: &'a str,
        _call: &'a Call,
        _budget: Duration,
    ) -> HookFuture<'a, String> {
        Box::pin(async move { Ok(replace_all(&self.rules(), text)) })
    }
}

/// The replacement list as a plugin: it serves itself at the transforms point.
struct ReplacePlugin(Arc<Replace>);

impl Plugin<Points> for ReplacePlugin {
    fn id(&self) -> &str {
        REPLACE
    }

    fn apply(&self, points: &Points) -> Result<Active, ApplyError> {
        Ok(Active::new().hold(
            points
                .transforms
                .serve(Arc::clone(&self.0) as Arc<dyn TransformHook>),
        ))
    }
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
    use super::*;
    use crate::community::Hooks;

    fn rule(spoken: &str, written: &str) -> Replacement {
        Replacement {
            spoken: spoken.into(),
            written: written.into(),
        }
    }

    fn call() -> Call {
        Call {
            session: None,
            source: anya_protocol::Source::Dictation,
            app: None,
            settings: Default::default(),
        }
    }

    #[test]
    fn the_longest_spoken_side_wins_and_what_is_written_is_not_matched_again() {
        let mut rules = vec![rule("new", "N"), rule("new line", "\n"), rule("a", "aa")];
        rules.sort_by_key(|r| Reverse(r.spoken.len()));
        assert_eq!(
            replace_all(&rules, "a new line, a new day"),
            "aa \n, aa N daay"
        );
        assert_eq!(replace_all(&rules, "é 你好 new"), "é 你好 N");
        assert_eq!(replace_all(&[], "same"), "same");
    }

    #[tokio::test]
    async fn the_replacement_list_applies_and_follows_its_file() {
        let dir = std::env::temp_dir().join(format!("anya-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("replacements.txt");
        let replace = Replace::new(file.clone());
        let say = |text: &'static str| {
            let replace = &replace;
            async move {
                replace
                    .transform(text, &call(), Duration::from_millis(20))
                    .await
            }
        };
        assert_eq!(
            say("anya").await,
            Ok("anya".to_owned()),
            "no file, no change"
        );

        std::fs::write(&file, "# names\nanya => Anya\nsmiley => :)\\t\n").expect("write");
        assert_eq!(say("hi anya smiley").await, Ok("hi Anya :)\t".to_owned()));

        std::fs::write(&file, "hi => hello there\n").expect("rewrite");
        assert_eq!(say("hi anya").await, Ok("hello there anya".to_owned()));

        std::fs::remove_file(&file).expect("remove");
        assert_eq!(say("hi").await, Ok("hi".to_owned()));
        let _ = std::fs::remove_dir_all(&dir);
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
        let dir = std::env::temp_dir().join("anya-no-replacements.txt");
        let points = Points::default();
        let mut rt = Runtime::new(points.clone(), plugins(dir, community.clone())).expect("rt");
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
        rt.reconcile(|id| id == REPLACE);
        assert_eq!(served(), ["replace"], "community-hooks is off");

        rt.reconcile(|id| id == REPLACE || id == COMMUNITY_HOOKS);
        assert_eq!(served(), ["replace", "early"]);

        community.set_host(hooks(&["early", "late"]));
        assert_eq!(
            served(),
            ["replace", "early", "late"],
            "a host change reaches the point"
        );

        rt.reconcile(|id| id == REPLACE);
        assert_eq!(served(), ["replace"], "off again: none of them run");
        community.set_host(hooks(&["other"]));
        assert_eq!(served(), ["replace"], "an off plugin is not served into");
    }
}
