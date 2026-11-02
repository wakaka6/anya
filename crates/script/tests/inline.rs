//! What a consumer of an inline plugin sees: results, failures, limits.

use std::path::Path;
use std::time::{Duration, Instant};

use anya_core::hooks::{Budgets, Call, HookError};
use anya_protocol::{Limits, Provide, SessionId, Source};
use anya_script::{InlinePlugin, ScriptError};

fn budgets() -> Budgets {
    Budgets::resolve(Limits::default(), Limits::default(), Limits::default())
}

fn call() -> Call {
    Call {
        session: Some(SessionId("s1".to_owned())),
        source: Source::Dictation,
        app: Some("com.example.app".to_owned()),
        settings: [("mode".to_owned(), "loud".to_owned())].into(),
    }
}

async fn load(src: &str) -> Result<InlinePlugin, ScriptError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("plugin.js");
    std::fs::write(&file, src).expect("write");
    InlinePlugin::load("test", &file, &budgets()).await
}

async fn run(plugin: &InlinePlugin, text: &str) -> Result<String, HookError> {
    let hook = plugin.transform().expect("transform");
    hook.transform(text, &call(), Duration::from_millis(100))
        .await
}

const MS: Duration = Duration::from_millis(100);

#[tokio::test]
async fn transform_returns_the_string() {
    let p = load("export function transform(t, ctx) { return t.toUpperCase() + ctx.source + ctx.session + ctx.app + ctx.settings.mode; }")
        .await
        .expect("load");
    assert_eq!(p.provides(), vec![Provide::Transform]);
    assert!(p.context().is_none());
    assert_eq!(
        run(&p, "hi ").await.expect("ok"),
        "HI dictations1com.example.apploud"
    );
}

#[tokio::test]
async fn state_is_kept_between_calls() {
    let p = load("let n = 0; export function transform() { n += 1; return String(n); }")
        .await
        .expect("load");
    assert_eq!(run(&p, "").await.expect("ok"), "1");
    assert_eq!(run(&p, "").await.expect("ok"), "2");
}

#[tokio::test]
async fn endless_loop_times_out_and_plugin_survives() {
    let p =
        load("export function transform(t) { if (t === 'spin') { while (true) {} } return t; }")
            .await
            .expect("load");
    let start = Instant::now();
    let hook = p.transform().expect("hook");
    let out = hook
        .transform("spin", &call(), Duration::from_millis(20))
        .await;
    assert_eq!(out, Err(HookError::Timeout));
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(run(&p, "ok").await.expect("ok"), "ok");
}

#[tokio::test]
async fn runaway_allocation_is_memory_and_plugin_survives() {
    let p = load(
        "export function transform(t) { if (t === 'eat') { const a = []; for (;;) a.push(new Array(100000).fill(1)); } return t; }",
    )
    .await
    .expect("load");
    let hook = p.transform().expect("hook");
    let out = hook.transform("eat", &call(), Duration::from_secs(5)).await;
    assert_eq!(out, Err(HookError::Memory));
    assert_eq!(run(&p, "ok").await.expect("ok"), "ok");
}

#[tokio::test]
async fn thrown_error_is_failed_with_message() {
    let p = load("export function transform() { throw new Error('nope'); }")
        .await
        .expect("load");
    assert_eq!(
        run(&p, "x").await,
        Err(HookError::Failed("nope".to_owned()))
    );
}

#[tokio::test]
async fn wrong_shapes_are_invalid() {
    let p = load("export function transform() { return 3; }")
        .await
        .expect("load");
    assert!(matches!(run(&p, "x").await, Err(HookError::Invalid(_))));

    let p = load("export function context(c) { return c.language === 'a' ? [] : c.language === 'b' ? { hotwords: 'x' } : { hotwords: ['a', 1] }; }")
        .await
        .expect("load");
    let hook = p.context().expect("hook");
    for lang in ["a", "b", "c"] {
        let out = hook.context(&call(), Some(lang), &[], MS).await;
        assert!(matches!(out, Err(HookError::Invalid(_))), "{lang}: {out:?}");
    }
}

#[tokio::test]
async fn modules_with_imports_are_refused() {
    for src in [
        "import x from 'y'; export function transform(t) { return t; }",
        "export function transform(t) { return require('fs'); }",
        "export async function transform(t) { return (await import('x')).default; }",
    ] {
        assert!(
            matches!(load(src).await, Err(ScriptError::Forbidden(_))),
            "{src}"
        );
    }
    // the words in strings and comments are fine
    load("// import\nexport function transform() { return 'require import'; }")
        .await
        .expect("load");
}

#[tokio::test]
async fn a_module_without_hooks_does_not_load() {
    assert!(matches!(
        load("export const x = 1;").await,
        Err(ScriptError::NoHooks)
    ));
    assert!(matches!(
        load("export function transform( {").await,
        Err(ScriptError::Load(_))
    ));
    assert!(matches!(
        load("while (true) {}").await,
        Err(ScriptError::LoadTimeout)
    ));
    let missing = InlinePlugin::load("t", Path::new("/no/such/file.js"), &budgets()).await;
    assert!(matches!(missing, Err(ScriptError::Read { .. })));
}

#[tokio::test]
async fn nothing_of_the_outside_world() {
    let p = load(
        "export function transform() { return [typeof fetch, typeof setTimeout, typeof globalThis['req' + 'uire'], typeof process, typeof console.log].join(','); }",
    )
    .await
    .expect("load");
    assert_eq!(
        run(&p, "").await.expect("ok"),
        "undefined,undefined,undefined,undefined,function"
    );
}

#[tokio::test]
async fn context_merges_into_extra() {
    let p = load(
        "export function context(c) { if (c.hotwords.length === 0) return; return { hotwords: [...c.hotwords, 'Anya'], language: c.source === 'dictation' ? 'de' : undefined }; }",
    )
    .await
    .expect("load");
    assert_eq!(p.provides(), vec![Provide::Context]);
    let hook = p.context().expect("hook");
    let none = hook.context(&call(), None, &[], MS).await.expect("ok");
    assert!(none.hotwords.is_empty() && none.language.is_none());
    let words = ["x".to_owned()];
    let some = hook
        .context(&call(), Some("en"), &words, MS)
        .await
        .expect("ok");
    assert_eq!(some.hotwords, vec!["x".to_owned(), "Anya".to_owned()]);
    assert_eq!(some.language.as_deref(), Some("de"));
}

#[tokio::test]
async fn async_results_are_awaited() {
    let p = load("export async function transform(t) { await null; return t + '!'; }")
        .await
        .expect("load");
    assert_eq!(run(&p, "a").await.expect("ok"), "a!");

    let p = load("export function transform() { return new Promise(() => {}); }")
        .await
        .expect("load");
    assert_eq!(run(&p, "a").await, Err(HookError::Timeout));

    let p = load("export async function transform() { throw new Error('late'); }")
        .await
        .expect("load");
    assert_eq!(
        run(&p, "a").await,
        Err(HookError::Failed("late".to_owned()))
    );
}
