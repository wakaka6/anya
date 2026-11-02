//! The hook runner as a dictation sees it: what a session opens with, what
//! text goes on, which actions run and in what order, and what is reported
//! when a plugin fails or runs out of time.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_core::engine::{Engine, Input, Output, Session, SessionParams};
use anya_core::hooks::{
    ActionHook, Actions, Budgets, Call, ContextEngine, ContextHook, Extra, HookError, HookFailure,
    HookFuture, Planned, Runs, TransformHook, chain_budget, gather, transform,
};
use anya_protocol::{Limits, Manifest, Provide, Source};
use tokio::sync::mpsc;

fn call() -> Call {
    Call {
        session: None,
        source: Source::Dictation,
        app: Some("com.example.Editor".to_owned()),
        settings: BTreeMap::new(),
    }
}

fn planned<H: ?Sized>(hook: Arc<H>, ms: u64) -> Planned<H> {
    Planned {
        hook,
        settings: BTreeMap::from([("k".to_owned(), "v".to_owned())]),
        budget: Duration::from_millis(ms),
    }
}

struct Ctx {
    id: &'static str,
    wait: Duration,
    answer: Result<Extra, HookError>,
}

impl ContextHook for Ctx {
    fn plugin(&self) -> &str {
        self.id
    }
    fn runs(&self) -> Runs {
        Runs::Host
    }
    fn context<'a>(
        &'a self,
        call: &'a Call,
        _: Option<&'a str>,
        _: &'a [String],
        _: Duration,
    ) -> HookFuture<'a, Extra> {
        Box::pin(async move {
            assert_eq!(call.settings.get("k").map(String::as_str), Some("v"));
            tokio::time::sleep(self.wait).await;
            self.answer.clone()
        })
    }
}

fn ctx(id: &'static str, ms: u64, answer: Result<Extra, HookError>) -> Arc<dyn ContextHook> {
    Arc::new(Ctx {
        id,
        wait: Duration::from_millis(ms),
        answer,
    })
}

fn extra(words: &[&str], language: Option<&str>) -> Result<Extra, HookError> {
    Ok(Extra {
        hotwords: words.iter().map(|w| (*w).to_owned()).collect(),
        language: language.map(str::to_owned),
    })
}

#[tokio::test(start_paused = true)]
async fn context_hooks_merge_in_order_and_run_side_by_side() {
    let hooks = vec![
        planned(ctx("a", 200, extra(&["Anya", "rquickjs"], None)), 300),
        planned(ctx("b", 200, extra(&["rquickjs", "Bun"], Some("en"))), 300),
        planned(ctx("c", 200, extra(&[], Some("ja"))), 300),
        planned(ctx("d", 0, Err(HookError::Failed("boom".to_owned()))), 300),
    ];
    let mut params = SessionParams {
        language: Some("zh-CN".to_owned()),
        hotwords: vec!["Anya".to_owned()],
        settings: BTreeMap::new(),
    };
    let started = tokio::time::Instant::now();
    let failures = gather(&hooks, &call(), &mut params).await;
    // Asked at once: the slowest hook's time, not the sum.
    assert!(started.elapsed() < Duration::from_millis(400));
    assert_eq!(params.hotwords, ["Anya", "rquickjs", "Bun"]);
    // The first hook in order that names a language sets it.
    assert_eq!(params.language.as_deref(), Some("en"));
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].plugin, "d");
    assert_eq!(failures[0].hook, Provide::Context);
    assert_eq!(failures[0].failure, HookFailure::Failed);
}

#[tokio::test(start_paused = true)]
async fn a_slow_context_hook_adds_nothing() {
    let hooks = vec![planned(
        ctx("slow", 5000, extra(&["late"], Some("en"))),
        300,
    )];
    let mut params = SessionParams::default();
    let failures = gather(&hooks, &call(), &mut params).await;
    assert_eq!(params, SessionParams::default());
    assert_eq!(failures[0].failure, HookFailure::Timeout);
}

struct Tf {
    id: &'static str,
    wait: Duration,
    f: fn(&str) -> Result<String, HookError>,
}

impl TransformHook for Tf {
    fn plugin(&self) -> &str {
        self.id
    }
    fn runs(&self) -> Runs {
        Runs::Inline
    }
    fn transform<'a>(&'a self, text: &'a str, _: &'a Call, _: Duration) -> HookFuture<'a, String> {
        Box::pin(async move {
            tokio::time::sleep(self.wait).await;
            (self.f)(text)
        })
    }
}

fn tf(
    id: &'static str,
    ms: u64,
    f: fn(&str) -> Result<String, HookError>,
) -> Planned<dyn TransformHook> {
    planned(
        Arc::new(Tf {
            id,
            wait: Duration::from_millis(ms),
            f,
        }) as Arc<dyn TransformHook>,
        1000,
    )
}

#[tokio::test(start_paused = true)]
async fn transforms_chain_and_a_failed_step_is_skipped() {
    let chain = vec![
        tf("upper", 0, |t| Ok(t.to_uppercase())),
        tf("broken", 0, |_| Err(HookError::Failed("no".to_owned()))),
        tf("huge", 0, |_| Ok("x".repeat(70_000))),
        tf("dot", 0, |t| Ok(format!("{t}."))),
    ];
    let (text, failures) =
        transform(&chain, "hello".to_owned(), &call(), Duration::from_secs(4)).await;
    assert_eq!(text, "HELLO.");
    let failed: Vec<_> = failures
        .iter()
        .map(|f| (f.plugin.as_str(), f.failure))
        .collect();
    assert_eq!(
        failed,
        [
            ("broken", HookFailure::Failed),
            ("huge", HookFailure::Invalid)
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn the_chain_budget_bounds_the_whole_chain() {
    let chain = vec![
        tf("slow", 900, |t| Ok(format!("{t}1"))),
        tf("slower", 900, |t| Ok(format!("{t}2"))),
        tf("last", 0, |t| Ok(format!("{t}3"))),
    ];
    let started = tokio::time::Instant::now();
    let (text, failures) =
        transform(&chain, "a".to_owned(), &call(), chain_budget(Some(1000))).await;
    // The first step fits; the second is cut at what is left, the third has
    // no time left.
    assert_eq!(text, "a1");
    assert!(started.elapsed() < Duration::from_millis(1100));
    let failed: Vec<_> = failures.iter().map(|f| f.plugin.as_str()).collect();
    assert_eq!(failed, ["slower", "last"]);
    assert!(failures.iter().all(|f| f.failure == HookFailure::Timeout));
}

struct Act {
    id: &'static str,
    wait: Duration,
    fail: bool,
    log: Arc<Mutex<Vec<String>>>,
}

impl ActionHook for Act {
    fn plugin(&self) -> &str {
        self.id
    }
    fn runs(&self) -> Runs {
        Runs::Host
    }
    fn act<'a>(&'a self, text: &'a str, _: &'a Call, _: Duration) -> HookFuture<'a, ()> {
        Box::pin(async move {
            tokio::time::sleep(self.wait).await;
            self.log
                .lock()
                .expect("log")
                .push(format!("{}:{text}", self.id));
            if self.fail {
                Err(HookError::Failed("disk full".to_owned()))
            } else {
                Ok(())
            }
        })
    }
}

#[tokio::test(start_paused = true)]
async fn actions_run_in_session_order_per_plugin_and_side_by_side_across() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let act = |id, ms, fail| {
        planned(
            Arc::new(Act {
                id,
                wait: Duration::from_millis(ms),
                fail,
                log: Arc::clone(&log),
            }) as Arc<dyn ActionHook>,
            10_000,
        )
    };
    let actions = vec![act("notes", 300, false), act("todo", 10, true)];
    let (runner, mut failed) = Actions::new();
    runner.dispatch(&actions, "one", &call());
    runner.dispatch(&actions, "two", &call());
    tokio::time::sleep(Duration::from_secs(1)).await;
    let log = log.lock().expect("log").clone();
    // A slow plugin holds up nobody else, and keeps its own order.
    assert_eq!(log, ["todo:one", "todo:two", "notes:one", "notes:two"]);
    for _ in 0..2 {
        let f = failed.recv().await.expect("failure");
        assert_eq!((f.plugin.as_str(), f.hook), ("todo", Provide::Action));
        assert_eq!(f.failure, HookFailure::Failed);
    }
}

#[tokio::test(start_paused = true)]
async fn an_action_past_its_budget_is_reported() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(Act {
        id: "stuck",
        wait: Duration::from_secs(60),
        fail: false,
        log,
    }) as Arc<dyn ActionHook>;
    let (runner, mut failed) = Actions::new();
    runner.dispatch(&[planned(hook, 1000)], "x", &call());
    let f = failed.recv().await.expect("failure");
    assert_eq!(f.failure, HookFailure::Timeout);
}

#[test]
fn budgets_take_the_user_over_the_plugin_and_stay_in_range() {
    let own = Limits {
        transform_ms: Some(4000),
        ..Limits::default()
    };
    let every = Limits {
        transform_ms: Some(1000),
        context_ms: Some(10),
        ..Limits::default()
    };
    let asked = Limits {
        action_ms: Some(999_999),
        inline_ms: Some(50),
        ..Limits::default()
    };
    let b = Budgets::resolve(own, every, asked);
    assert_eq!(b.transform, Duration::from_millis(4000));
    // Below its range: raised to the least allowed.
    assert_eq!(b.context, Duration::from_millis(50));
    // Above its range: cut to the cap, which no one can raise.
    assert_eq!(b.action, Duration::from_millis(60_000));
    assert_eq!(b.inline, Duration::from_millis(50));
    assert_eq!(b.inline_memory, 16 << 20);
    assert_eq!(b.of(Provide::Transform, Runs::Inline), b.inline);
    assert_eq!(b.of(Provide::Transform, Runs::Host), b.transform);
    assert_eq!(chain_budget(None), Duration::from_millis(4000));
    assert_eq!(chain_budget(Some(60_000)), Duration::from_millis(15_000));
}

/// An engine that reports what it opened with and echoes how many audio
/// chunks it got on finish.
struct Probe {
    manifest: Manifest,
    opened: mpsc::UnboundedSender<SessionParams>,
}

impl Engine for Probe {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn open(&self, params: SessionParams) -> Session {
        let _ = self.opened.send(params);
        let (input, mut rx) = mpsc::channel(16);
        let (tx, output) = mpsc::channel(16);
        tokio::spawn(async move {
            let _ = tx.send(Output::Ready).await;
            let mut chunks = 0;
            while let Some(item) = rx.recv().await {
                match item {
                    Input::Audio(_) => chunks += 1,
                    Input::Finish => {
                        let _ = tx
                            .send(Output::Result(anya_core::engine::final_text(format!(
                                "{chunks}"
                            ))))
                            .await;
                        return;
                    }
                }
            }
        });
        Session { input, output }
    }
}

#[tokio::test(start_paused = true)]
async fn a_context_engine_opens_with_the_hooks_answers_and_keeps_early_audio() {
    let (opened, mut opened_rx) = mpsc::unbounded_channel();
    let inner = Arc::new(Probe {
        manifest: Manifest {
            id: "probe".to_owned(),
            title: "Probe".into(),
            blurb: Default::default(),
            docs: None,
            streaming: true,
            fields: Vec::new(),
        },
        opened,
    });
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reported);
    let engine = ContextEngine::new(
        inner,
        vec![
            planned(ctx("words", 200, extra(&["Bun"], Some("en"))), 300),
            planned(
                ctx("dead", 0, Err(HookError::Unavailable("gone".to_owned()))),
                300,
            ),
        ],
        call(),
        Arc::new(move |f| sink.lock().expect("sink").extend(f)),
    );
    assert_eq!(engine.manifest().id, "probe");
    let mut session = engine.open(SessionParams::default());
    // Audio sent while the hooks are still answering waits for the engine.
    for _ in 0..3 {
        session
            .input
            .send(Input::Audio(anya_core::audio::Chunk(vec![0; 1280])))
            .await
            .expect("audio");
    }
    session.input.send(Input::Finish).await.expect("finish");
    let params = opened_rx.recv().await.expect("opened");
    assert_eq!(params.hotwords, ["Bun"]);
    assert_eq!(params.language.as_deref(), Some("en"));
    assert!(matches!(session.output.recv().await, Some(Output::Ready)));
    match session.output.recv().await {
        Some(Output::Result(r)) => assert_eq!(r.text, "3"),
        other => panic!("expected the final result, got {other:?}"),
    }
    let reported = reported.lock().expect("reported");
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].failure, HookFailure::Unavailable);
}
