//! The real Bun host with real plugin packages, driven through `Hub` the way
//! a dictation trigger drives it. Skipped, with a message, without a usable
//! `bun` on PATH.

mod common;

use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{Engine, SessionParams};
use anya_core::hub::{Hub, HubInfo, Subscription};
use anya_plugin_host::{
    BunPath, MIN_BUN_VERSION, PluginDirs, PluginHost, PluginState, Registry, Store,
};
use anya_protocol::{EndReason, ErrorCode, Frame, Source, contract};
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
                              "anya": {"engine": "./engine.js"}})
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
