//! Which hooks one dictation runs, in what order and with what budgets
//! (docs/decisions/0015, 0016).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anya_config::Config;
use anya_core::hooks::{ActionHook, ContextHook, Planned, Runs, TransformHook};
use anya_protocol::Provide;
use anya_runtime::HookInfo;

use crate::builtin::Points;
use crate::community::Declared;

/// The hooks a dictation runs.
pub struct Plan {
    /// Asked before the session opens, in the user's order.
    pub contexts: Vec<Planned<dyn ContextHook>>,
    /// The chain on the final text, in the user's order.
    pub transforms: Vec<Planned<dyn TransformHook>>,
    /// Run beside the paste, in plugin-id order.
    pub actions: Vec<Planned<dyn ActionHook>>,
    /// The whole chain's budget.
    pub chain: Duration,
}

impl Plan {
    /// The hooks as the settings list them, in the order they run.
    pub fn infos(&self) -> Vec<HookInfo> {
        let info = |plugin: &str, hook, runs| HookInfo {
            plugin: plugin.to_owned(),
            hook,
            runs,
        };
        let contexts = self
            .contexts
            .iter()
            .map(|p| info(p.hook.plugin(), Provide::Context, p.hook.runs()));
        let transforms = self
            .transforms
            .iter()
            .map(|p| info(p.hook.plugin(), Provide::Transform, p.hook.runs()));
        let actions = self
            .actions
            .iter()
            .map(|p| info(p.hook.plugin(), Provide::Action, p.hook.runs()));
        contexts.chain(transforms).chain(actions).collect()
    }
}

/// What `points` serve now, planned by `cfg`: plugins in `[plugins] order`,
/// the unlisted after them by id; each with its settings and its budget
/// (`declared` holds the budgets packages ask for, by plugin id).
pub fn plan(cfg: &Config, points: &Points, declared: &BTreeMap<String, Declared>) -> Plan {
    Plan {
        contexts: ordered(
            cfg,
            declared,
            Provide::Context,
            points
                .contexts
                .all()
                .into_iter()
                .map(|h| (h.plugin().to_owned(), h.runs(), h))
                .collect(),
        ),
        transforms: ordered(
            cfg,
            declared,
            Provide::Transform,
            points
                .transforms
                .all()
                .into_iter()
                .map(|h| (h.plugin().to_owned(), h.runs(), h))
                .collect(),
        ),
        actions: ordered(
            cfg,
            declared,
            Provide::Action,
            points
                .actions
                .all()
                .into_iter()
                .map(|h| (h.plugin().to_owned(), h.runs(), h))
                .collect(),
        ),
        chain: cfg.plugins.chain_budget(),
    }
}

fn ordered<H: ?Sized>(
    cfg: &Config,
    declared: &BTreeMap<String, Declared>,
    kind: Provide,
    mut hooks: Vec<(String, Runs, Arc<H>)>,
) -> Vec<Planned<H>> {
    let plugins = &cfg.plugins;
    hooks.sort_by(|a, b| {
        plugins
            .rank(&a.0)
            .cmp(&plugins.rank(&b.0))
            .then_with(|| a.0.cmp(&b.0))
    });
    hooks
        .into_iter()
        .map(|(id, runs, hook)| Planned {
            settings: plugins.settings_of(&id),
            budget: plugins
                .budgets(&id, declared.get(&id).map(|d| d.limits).unwrap_or_default())
                .of(kind, runs),
            hook,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anya_core::hooks::{Call, HookFuture};
    use anya_protocol::Limits;

    struct Named(&'static str, Runs);

    impl TransformHook for Named {
        fn plugin(&self) -> &str {
            self.0
        }
        fn runs(&self) -> Runs {
            self.1
        }
        fn transform<'a>(
            &'a self,
            text: &'a str,
            _: &'a Call,
            _: Duration,
        ) -> HookFuture<'a, String> {
            Box::pin(async move { Ok(text.to_owned()) })
        }
    }

    impl ContextHook for Named {
        fn plugin(&self) -> &str {
            self.0
        }
        fn runs(&self) -> Runs {
            self.1
        }
        fn context<'a>(
            &'a self,
            _: &'a Call,
            _: Option<&'a str>,
            _: &'a [String],
            _: Duration,
        ) -> HookFuture<'a, anya_core::hooks::Extra> {
            Box::pin(async move { Ok(Default::default()) })
        }
    }

    fn serve_transforms(
        points: &Points,
        hooks: &[(&'static str, Runs)],
    ) -> Vec<anya_plugin_rt::Registration> {
        hooks
            .iter()
            .map(|(id, runs)| points.transforms.serve(Arc::new(Named(id, *runs))))
            .collect()
    }

    fn ids(plan: &Plan) -> Vec<&str> {
        plan.transforms.iter().map(|p| p.hook.plugin()).collect()
    }

    #[test]
    fn hooks_run_in_the_users_order_then_the_rest_by_id() {
        let points = Points::default();
        let _held = serve_transforms(
            &points,
            &[
                ("zeta", Runs::Host),
                ("alpha", Runs::Inline),
                ("replace", Runs::Builtin),
                ("mid", Runs::Host),
            ],
        );
        let mut cfg = Config::default();
        let none = BTreeMap::new();
        assert_eq!(
            ids(&plan(&cfg, &points, &none)),
            ["alpha", "mid", "replace", "zeta"]
        );

        cfg.plugins.order = vec!["zeta".into(), "replace".into(), "gone".into()];
        let p = plan(&cfg, &points, &none);
        assert_eq!(ids(&p), ["zeta", "replace", "alpha", "mid"]);
        assert_eq!(
            p.infos()
                .iter()
                .map(|i| (i.plugin.as_str(), i.hook, i.runs))
                .collect::<Vec<_>>(),
            [
                ("zeta", Provide::Transform, Runs::Host),
                ("replace", Provide::Transform, Runs::Builtin),
                ("alpha", Provide::Transform, Runs::Inline),
                ("mid", Provide::Transform, Runs::Host),
            ]
        );
    }

    #[test]
    fn each_hook_has_its_plugins_settings_and_its_budget() {
        let points = Points::default();
        let _held = serve_transforms(&points, &[("polish", Runs::Host), ("tidy", Runs::Inline)]);
        let ctx = points.contexts.serve(Arc::new(Named("polish", Runs::Host)));
        let mut cfg = Config::default();
        cfg.plugins.settings.insert(
            "polish".into(),
            [("api_key".to_owned(), "k".to_owned())].into(),
        );
        cfg.plugins.limits.plugins.insert(
            "polish".into(),
            Limits {
                transform_ms: Some(4000),
                ..Limits::default()
            },
        );
        let declared = BTreeMap::from([(
            "tidy".to_owned(),
            Declared {
                limits: Limits {
                    inline_ms: Some(50),
                    ..Limits::default()
                },
            },
        )]);
        let p = plan(&cfg, &points, &declared);
        let polish = &p.transforms[0];
        assert_eq!(polish.hook.plugin(), "polish");
        assert_eq!(polish.settings["api_key"], "k");
        assert_eq!(polish.budget, Duration::from_millis(4000));
        let tidy = &p.transforms[1];
        assert!(tidy.settings.is_empty());
        assert_eq!(tidy.budget, Duration::from_millis(50), "the package's ask");
        assert_eq!(p.contexts[0].budget, Duration::from_millis(300));
        assert_eq!(p.chain, Duration::from_millis(4000));
        drop(ctx);
    }
}
