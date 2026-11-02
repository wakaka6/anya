//! The real Bun host with real plugin packages, driven through `Hub` the way
//! a dictation trigger drives it. Skipped, with a message, without a usable
//! `bun` on PATH.

mod common;

use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{Engine, SessionParams};
use anya_core::hooks::{ActionHook, HookError, Runs, TransformHook};
use anya_core::hub::{Hub, HubInfo, Subscription};
use anya_plugin_host::{
    BunPath, MIN_BUN_VERSION, PluginDirs, PluginHost, PluginState, Registry, Store,
};
use anya_protocol::{EndReason, ErrorCode, Frame, Provide, Source, contract};
use common::{example, index, tgz, tgz_of_dir};
use serde_json::json;

const SETTLE: Duration = Duration::from_secs(30);

fn hub() -> Hub {
    Hub::new(HubInfo {
        version: "0.1.0".to_owned(),
        session_prefix: "anya-test".to_owned(),
    })
}

fn loud() -> Chunk {
    Chunk([0x00, 0x40].repeat(640))
}

/// A plugin package whose engine is `engine_js`.
fn package(name: &str, engine_js: &str) -> Vec<u8> {
    let package_json = json!({"name": name, "version": "1.0.0", "type": "module",
                              "anya": {"host": "./engine.js"}})
    .to_string();
    tgz(&[
        ("package.json", package_json.as_bytes()),
        ("engine.js", engine_js.as_bytes()),
    ])
}

/// Installs and enables each `(id, tarball)` through a local registry index.
async fn install(dir: &tempfile::TempDir, plugins: &[(&str, Vec<u8>)]) -> Store {
    let mut entries = Vec::new();
    for (id, bytes) in plugins {
        let file = dir.path().join(format!("{id}.tgz"));
        std::fs::write(&file, bytes).expect("write tarball");
        let doc = index(id, "1.0.0", &format!("file://{}", file.display()), bytes);
        entries.push(doc["plugins"][0].clone());
    }
    let index_file = dir.path().join("index.json");
    std::fs::write(
        &index_file,
        json!({"version": 1, "plugins": entries}).to_string(),
    )
    .expect("write index");
    let index = Registry::fetch(&format!("file://{}", index_file.display()))
        .await
        .expect("index");
    let store = Store::new(PluginDirs::under(dir.path()));
    for (id, _) in plugins {
        store
            .install(&index, id, "1.0.0", "0.1.0")
            .await
            .expect("install");
        store.enable(id).expect("enable");
    }
    store
}

#[allow(clippy::print_stderr)] // the skip message
async fn bun(dir: &tempfile::TempDir) -> Option<BunPath> {
    let bun = BunPath::detect(&dir.path().join("runtime")).await;
    if bun.is_none() {
        eprintln!("skipped: no bun {MIN_BUN_VERSION} or newer on PATH");
    }
    bun
}

fn read(sub: &mut Subscription) -> Vec<Frame> {
    let mut frames = std::mem::take(&mut sub.snapshot);
    while let Ok(f) = sub.frames.try_recv() {
        frames.push(f);
    }
    frames
}

async fn started(sub: &mut Subscription) {
    loop {
        let frame = sub.frames.recv().await.expect("frames");
        sub.snapshot.push(frame.clone());
        if matches!(frame, Frame::SessionStart { .. }) {
            return;
        }
    }
}

fn engine(host: &PluginHost, id: &str) -> std::sync::Arc<dyn Engine> {
    host.engines()
        .into_iter()
        .find(|e| e.manifest().id == id)
        .unwrap_or_else(|| panic!("no engine {id}: {:?}", host.status()))
}

#[tokio::test]
async fn a_streaming_plugin_session_reads_as_promised() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let echo = tgz_of_dir(&example("echo-engine"));
    let store = install(&dir, &[("echo-engine", echo)]).await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await, "{:?}", host.status());

    let engine = engine(&host, "echo");
    assert!(engine.manifest().streaming);
    let hub = hub();
    let mut sub = hub.subscribe();
    let params = SessionParams {
        settings: [("prefix".to_owned(), "heard ".to_owned())].into(),
        ..SessionParams::default()
    };
    let session = hub.open(&*engine, Source::Dictation, params);
    started(&mut sub).await;
    for _ in 0..3 {
        assert!(session.send(loud()).await);
    }
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(
        (outcome.reason, outcome.text.as_str()),
        (EndReason::Final, "heard 3840 bytes")
    );

    let frames = read(&mut sub);
    assert_eq!(contract::check(&frames), Ok(()));
    assert!(frames.iter().any(
        |f| matches!(f, Frame::SessionStart { engine, streaming: true, .. } if engine == "echo")
    ));
    let results: Vec<(&str, bool)> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Result { recognition, .. } => {
                Some((recognition.text.as_str(), recognition.is_final))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [
            ("heard 1280 bytes", false),
            ("heard 2560 bytes", false),
            ("heard 3840 bytes", false),
            ("heard 3840 bytes", true),
        ]
    );
    host.shutdown().await;
}

#[tokio::test]
async fn a_plugin_that_fails_to_open_ends_the_session_with_its_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(
        &dir,
        &[("echo-engine", tgz_of_dir(&example("echo-engine")))],
    )
    .await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await);

    let params = SessionParams {
        settings: [("fail".to_owned(), "connect".to_owned())].into(),
        ..SessionParams::default()
    };
    let session = hub().open(&*engine(&host, "echo"), Source::Dictation, params);
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(outcome.reason, EndReason::Error);
    assert_eq!(outcome.error, Some(ErrorCode::Connect));
    assert_eq!(outcome.detail.as_deref(), Some("echo was told to fail"));
    host.shutdown().await;
}

const NOISY: &str = r#"
import { defineEngine } from "@wakaka6/anya-plugin";
console.log("loading noisy");
process.stdout.write("raw text on stdout\n");
export default defineEngine({
  manifest: { id: "noisy", title: "Noisy", streaming: false },
  open(_, session) {
    console.log("opening");
    session.on("audio", () => { throw new Error("cannot decode"); });
    session.on("finish", () => session.result({ text: "never", final: true }));
  },
});
"#;

const CRASHY: &str = r#"
import { defineEngine } from "@wakaka6/anya-plugin";
export default defineEngine({
  manifest: { id: "crashy", title: "Crashy", streaming: false },
  open(_, session) {
    session.on("audio", () => process.exit(3));
  },
});
"#;

const STUCK: &str = r#"
import { defineEngine } from "@wakaka6/anya-plugin";
await new Promise(() => {});
export default defineEngine({ manifest: { id: "stuck", title: "Stuck", streaming: false }, open() {} });
"#;

// Loops on its first audio without ever yielding, writing all the while.
const CHATTY: &str = r#"
import { defineEngine } from "@wakaka6/anya-plugin";
export default defineEngine({
  manifest: { id: "chatty", title: "Chatty", streaming: false },
  open(_, session) {
    session.on("audio", () => { for (;;) console.log("busy"); });
  },
});
"#;

#[tokio::test]
async fn a_plugin_exception_ends_only_its_session_and_console_output_stays_off_the_protocol() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(
        &dir,
        &[
            ("noisy", package("noisy", NOISY)),
            ("echo-engine", tgz_of_dir(&example("echo-engine"))),
        ],
    )
    .await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await, "{:?}", host.status());

    let hub = hub();
    let healthy = hub.open(
        &*engine(&host, "echo"),
        Source::Dictation,
        SessionParams::default(),
    );
    let broken = hub.open(
        &*engine(&host, "noisy"),
        Source::Dictation,
        SessionParams::default(),
    );
    assert!(broken.send(loud()).await);
    assert!(healthy.send(loud()).await);

    let outcome = broken.finish(Duration::from_secs(10)).await;
    assert_eq!(outcome.reason, EndReason::Error);
    assert_eq!(outcome.error, Some(ErrorCode::Upstream));
    assert_eq!(outcome.detail.as_deref(), Some("cannot decode"));

    let outcome = healthy.finish(Duration::from_secs(10)).await;
    assert_eq!(
        (outcome.reason, outcome.text.as_str()),
        (EndReason::Final, "1280 bytes")
    );
    assert!(host.running());
    host.shutdown().await;
}

#[tokio::test]
async fn a_crashed_host_ends_its_sessions_with_an_error_and_comes_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(
        &dir,
        &[
            ("crashy", package("crashy", CRASHY)),
            ("echo-engine", tgz_of_dir(&example("echo-engine"))),
        ],
    )
    .await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await);

    let hub = hub();
    let doomed = hub.open(
        &*engine(&host, "crashy"),
        Source::Dictation,
        SessionParams::default(),
    );
    assert!(doomed.send(loud()).await);
    let outcome = doomed.finish(Duration::from_secs(10)).await;
    assert_eq!(
        outcome.reason,
        EndReason::Error,
        "no fallback to another engine"
    );
    assert_eq!(outcome.error, Some(ErrorCode::Upstream));

    assert!(
        host.settled(SETTLE).await,
        "the host restarts: {:?}",
        host.status()
    );
    let session = hub.open(
        &*engine(&host, "echo"),
        Source::Dictation,
        SessionParams::default(),
    );
    assert!(session.send(loud()).await);
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(
        (outcome.reason, outcome.text.as_str()),
        (EndReason::Final, "1280 bytes")
    );
    host.shutdown().await;
}

#[tokio::test]
async fn a_stuck_plugin_does_not_hold_up_the_others() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(
        &dir,
        &[
            ("stuck", package("stuck", STUCK)),
            ("echo-engine", tgz_of_dir(&example("echo-engine"))),
        ],
    )
    .await;
    let host = PluginHost::start(store.dirs().clone(), bun);

    // The echo plugin is usable while the stuck one is still loading.
    let mut changes = host.changes();
    let echo_ready = |host: &PluginHost| {
        host.status()
            .iter()
            .any(|s| s.id == "echo-engine" && s.state == PluginState::Ready)
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !echo_ready(&host) {
            changes.changed().await.expect("changes");
        }
    })
    .await
    .expect("echo loads");
    let stuck = host
        .status()
        .into_iter()
        .find(|s| s.id == "stuck")
        .expect("stuck");
    assert_eq!(stuck.state, PluginState::Loading);

    let session = hub().open(
        &*engine(&host, "echo"),
        Source::Dictation,
        SessionParams::default(),
    );
    assert!(session.send(loud()).await);
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(outcome.reason, EndReason::Final);
    host.shutdown().await;
}

#[tokio::test]
async fn a_plugin_that_loops_while_writing_is_killed_and_its_session_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(&dir, &[("chatty", package("chatty", CHATTY))]).await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await);
    let mut stalls = host.stalls();

    let session = hub().open(
        &*engine(&host, "chatty"),
        Source::Dictation,
        SessionParams::default(),
    );
    assert!(session.send(loud()).await);
    let stalled = tokio::time::timeout(Duration::from_secs(15), stalls.recv())
        .await
        .expect("the watchdog kills it")
        .expect("stalls");
    assert_eq!(stalled, "chatty");
    let outcome = session.finish(Duration::from_secs(5)).await;
    assert_eq!(outcome.reason, EndReason::Error);
    host.shutdown().await;
}

#[tokio::test]
async fn without_a_running_host_a_session_fails_at_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(
        &dir,
        &[("echo-engine", tgz_of_dir(&example("echo-engine")))],
    )
    .await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await);
    let stale = engine(&host, "echo");

    store.disable("echo-engine").expect("disable");
    host.reload();
    assert!(host.settled(SETTLE).await);
    assert!(
        host.engines().is_empty(),
        "a disabled plugin offers no engine"
    );
    assert!(!host.running(), "nothing enabled, no process");

    let session = hub().open(&*stale, Source::Dictation, SessionParams::default());
    let outcome = session.finish(Duration::from_secs(5)).await;
    assert_eq!(
        (outcome.reason, outcome.error),
        (EndReason::Error, Some(ErrorCode::Connect))
    );
    host.shutdown().await;
}

fn call(settings: &[(&str, &str)]) -> anya_core::hooks::Call {
    anya_core::hooks::Call {
        session: None,
        source: Source::Dictation,
        app: None,
        settings: settings
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

fn transform_of(host: &PluginHost, id: &str) -> std::sync::Arc<dyn TransformHook> {
    host.transforms()
        .into_iter()
        .find(|h| h.plugin() == id)
        .unwrap_or_else(|| panic!("no transform {id}: {:?}", host.status()))
}

fn action_of(host: &PluginHost, id: &str) -> std::sync::Arc<dyn ActionHook> {
    host.actions()
        .into_iter()
        .find(|h| h.plugin() == id)
        .unwrap_or_else(|| panic!("no action {id}: {:?}", host.status()))
}

const SPINNER: &str = r#"
import { defineEngine } from "@wakaka6/anya-plugin";
export default defineEngine({
  manifest: { id: "spinner", title: "Spinner", streaming: false },
  open(_, session) {
    session.on("audio", () => { while (true) {} });
  },
});
"#;

#[tokio::test]
async fn a_plugin_that_stops_answering_is_killed_and_the_others_go_on() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(
        &dir,
        &[
            ("spinner", package("spinner", SPINNER)),
            ("echo-engine", tgz_of_dir(&example("echo-engine"))),
        ],
    )
    .await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await, "{:?}", host.status());
    let mut stalls = host.stalls();

    let hub = hub();
    let healthy = hub.open(
        &*engine(&host, "echo"),
        Source::Dictation,
        SessionParams::default(),
    );
    assert!(healthy.send(loud()).await);
    let stuck = hub.open(
        &*engine(&host, "spinner"),
        Source::Dictation,
        SessionParams::default(),
    );
    let blocked = std::time::Instant::now();
    assert!(stuck.send(loud()).await);

    let outcome = stuck.finish(Duration::from_secs(10)).await;
    let took = blocked.elapsed();
    assert_eq!(outcome.reason, EndReason::Error);
    assert_eq!(outcome.error, Some(ErrorCode::Upstream));
    assert_eq!(
        outcome.detail.as_deref(),
        Some("the plugin stopped answering")
    );
    assert!(took < Duration::from_secs(6), "killed after {took:?}");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stalls.recv())
            .await
            .expect("announced")
            .expect("open"),
        "spinner"
    );

    // The other plugin's session never noticed.
    assert!(healthy.send(loud()).await);
    let outcome = healthy.finish(Duration::from_secs(10)).await;
    assert_eq!(
        (outcome.reason, outcome.text.as_str()),
        (EndReason::Final, "2560 bytes")
    );
    assert!(stalls.try_recv().is_err(), "only the stuck plugin stalled");

    // The stuck plugin comes back after the backoff.
    assert!(host.settled(SETTLE).await, "{:?}", host.status());
    host.shutdown().await;
}

#[tokio::test]
async fn host_hooks_answer_and_a_budget_spent_leaves_the_plugin_working() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(&dir, &[("hooks", tgz_of_dir(&example("hooks")))]).await;
    assert!(store.needs_bun().expect("needs"));
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await, "{:?}", host.status());
    assert_eq!(
        host.status()[0].provides,
        [Provide::Transform, Provide::Action]
    );
    assert!(host.engines().is_empty());
    assert!(host.contexts().is_empty());

    let transform = transform_of(&host, "hooks");
    assert_eq!(transform.runs(), Runs::Host);
    let tagged = call(&[("suffix", "!")]);
    let text = transform
        .transform("  hello   world ", &tagged, Duration::from_secs(2))
        .await
        .expect("answers");
    assert_eq!(text, "hello world!");

    let spent = std::time::Instant::now();
    let err = transform
        .transform("slow", &tagged, Duration::from_millis(200))
        .await
        .expect_err("past its budget");
    assert_eq!(err, HookError::Timeout);
    assert!(spent.elapsed() < Duration::from_secs(2));

    let text = transform
        .transform("again", &tagged, Duration::from_secs(2))
        .await
        .expect("still works");
    assert_eq!(text, "again!");
    assert!(!host.status().iter().any(|s| s.state != PluginState::Ready));
    host.shutdown().await;
}

#[tokio::test]
async fn actions_do_their_work_and_a_throwing_one_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(&dir, &[("hooks", tgz_of_dir(&example("hooks")))]).await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await);
    let action = action_of(&host, "hooks");

    let notes = dir.path().join("notes.txt");
    let with_file = call(&[("file", &notes.display().to_string())]);
    action
        .act("first thing", &with_file, Duration::from_secs(2))
        .await
        .expect("writes");
    action
        .act("second thing", &with_file, Duration::from_secs(2))
        .await
        .expect("appends");
    assert_eq!(
        std::fs::read_to_string(&notes).expect("notes"),
        "first thing\nsecond thing\n"
    );

    let err = action
        .act("boom", &with_file, Duration::from_secs(2))
        .await
        .expect_err("throws");
    assert_eq!(err, HookError::Failed("action was told to fail".to_owned()));
    action
        .act("after", &with_file, Duration::from_secs(2))
        .await
        .expect("the plugin goes on");
    host.shutdown().await;
}

const HINTS: &str = r#"
import { defineContext, defineTransform } from "@wakaka6/anya-plugin";
export default defineContext({
  context: ({ settings, hotwords }) => settings.shape === "wrong"
    ? { hotwords: "not a list" }
    : { hotwords: [...hotwords.map((w) => w.toUpperCase()), "extra"], language: "fr" },
});
export const spare = defineTransform({ transform: (t) => t });
"#;

#[tokio::test]
async fn a_context_hook_answers_and_a_wrong_shape_is_a_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(&dir, &[("hints", package("hints", HINTS))]).await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await, "{:?}", host.status());
    let hook = host.contexts().pop().expect("a context hook");
    assert_eq!(hook.plugin(), "hints");

    let words = ["rust".to_owned()];
    let extra = hook
        .context(&call(&[]), Some("en"), &words, Duration::from_secs(2))
        .await
        .expect("answers");
    assert_eq!(extra.hotwords, ["RUST", "extra"]);
    assert_eq!(extra.language.as_deref(), Some("fr"));

    let err = hook
        .context(
            &call(&[("shape", "wrong")]),
            None,
            &[],
            Duration::from_secs(2),
        )
        .await
        .expect_err("wrong shape");
    assert!(matches!(err, HookError::Failed(_)), "{err:?}");
    host.shutdown().await;
}

const TWINS: &str = r#"
import { defineTransform } from "@wakaka6/anya-plugin";
export const a = defineTransform({ transform: (t) => t });
export const b = defineTransform({ transform: (t) => t });
"#;

#[tokio::test]
async fn a_package_with_two_hooks_of_one_kind_does_not_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(&dir, &[("twins", package("twins", TWINS))]).await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await);
    let status = host.status();
    assert!(
        matches!(&status[0].state, PluginState::Failed(why) if why.contains("two transform")),
        "{status:?}"
    );
    assert!(host.transforms().is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn a_hook_call_with_no_process_is_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(bun) = bun(&dir).await else { return };
    let store = install(&dir, &[("hooks", tgz_of_dir(&example("hooks")))]).await;
    let host = PluginHost::start(store.dirs().clone(), bun);
    assert!(host.settled(SETTLE).await);
    let stale = transform_of(&host, "hooks");

    store.disable("hooks").expect("disable");
    host.reload();
    assert!(host.settled(SETTLE).await);
    let err = stale
        .transform("late", &call(&[]), Duration::from_secs(1))
        .await
        .expect_err("no process");
    assert!(matches!(err, HookError::Unavailable(_)), "{err:?}");
    host.shutdown().await;
}

#[tokio::test]
async fn an_inline_only_package_starts_no_bun() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = install(&dir, &[("inline", tgz_of_dir(&example("inline")))]).await;
    assert!(!store.needs_bun().expect("needs"));

    // A Bun that cannot start would show up as a plugin that never loads.
    let host = PluginHost::start(
        store.dirs().clone(),
        BunPath(dir.path().join("no-such-bun")),
    );
    assert!(host.settled(Duration::from_secs(5)).await);
    assert!(host.status().is_empty());
    assert!(!host.running());
    assert!(host.transforms().is_empty());
    host.shutdown().await;
}
