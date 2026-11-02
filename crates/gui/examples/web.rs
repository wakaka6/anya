//! Serves the settings page and the tray panel against a fake Anya, for
//! trying the page in a browser:
//!
//! ```bash
//! cargo run -p anya-gui --example web            # picks a port
//! cargo run -p anya-gui --example web 127.0.0.1:7600
//! ```
//!
//! It prints the sign-in link; open it, and `/panel.html` for the panel.
//! Every few seconds the fake records a dictation, and a paired iPad
//! dictates through the gateway, so the live states and the route page show.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anya_config::EngineRole;
use anya_core::hooks::HookFailure;
use anya_gui::{Shell, ShellKind, web};
use anya_protocol::{Field, FieldKind, Provide, Scope, SessionId, Source};
use anya_runtime::{
    Delivered, Dictation, Entry, InstalledPlugin, Lan, LiveRoute, Origin, PairedClient, PluginLoad,
    ROUTE_LOG, RouteEvent, Sink, Step,
};

#[path = "../src/web/fake.rs"]
mod fake;

use fake::FakeControl;

/// A browser tab: nothing the desktop shell does is available.
struct BrowserShell;

impl Shell for BrowserShell {
    fn kind(&self) -> ShellKind {
        ShellKind::Web
    }

    fn open_url(&self, _url: &str) -> Result<(), String> {
        Err("a browser opens links itself".to_owned())
    }

    fn pick_file(&self, _start: Option<&Path>) -> Result<Option<PathBuf>, String> {
        Err("a browser cannot hand over a path".to_owned())
    }

    fn show_settings(&self) {}

    fn quit(&self) {}
}

const LINES: &[&str] = &[
    "Remind me to call the station at nine.",
    "下午三点开会，记得带上季度报表。",
    "The quarterly numbers look good.",
];

const REMOTE_LINES: &[&str] = &[
    "明天下午三点开会，记得带上周报。",
    "Add oat milk to the shopping list.",
];

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// One pretend session on the route page: it opens, sends audio a second at
/// a time with the text growing, and ends with its final text.
struct Pretend {
    session: SessionId,
    origin: Origin,
    engine: &'static str,
    sinks: Vec<Sink>,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

impl Pretend {
    fn log(&self, control: &FakeControl, step: Step) {
        let mut route = lock(&control.route);
        route.log.push(RouteEvent {
            seq: SEQ.fetch_add(1, Ordering::Relaxed) + 1,
            at: now_ms(),
            session: self.session.clone(),
            origin: self.origin.clone(),
            engine: self.engine.to_owned(),
            step,
        });
        let over = route.log.len().saturating_sub(ROUTE_LOG);
        route.log.drain(..over);
    }

    fn show(&self, control: &FakeControl, level: f32, text: &str) {
        let mut route = lock(&control.route);
        let live = LiveRoute {
            session: self.session.clone(),
            origin: self.origin.clone(),
            engine: self.engine.to_owned(),
            streaming: true,
            level,
            text: text.to_owned(),
            sinks: self.sinks.clone(),
        };
        match route.live.iter_mut().find(|r| r.session == self.session) {
            Some(r) => *r = live,
            None => route.live.push(live),
        }
    }

    /// Plays `line` in parts; `heard` sees the text so far.
    async fn play(&self, control: &FakeControl, line: &str, mut heard: impl FnMut(&str)) {
        self.log(control, Step::Start { streaming: true });
        let words: Vec<&str> = line.split_inclusive(' ').collect();
        let parts = if words.len() > 1 {
            words
        } else {
            line.split_inclusive('，').collect()
        };
        let mut live = String::new();
        let mut logged = String::new();
        let mut ticks = 0_u32;
        for part in parts {
            live.push_str(part);
            heard(live.trim());
            for _ in 0..4 {
                ticks += 1;
                let level = 0.35 + 0.3 * (ticks as f32 * 0.9).sin().abs();
                self.show(control, level, live.trim());
                if ticks.is_multiple_of(10) {
                    self.log(control, Step::Audio { ms: 1000 });
                    if logged != live.trim() {
                        logged = live.trim().to_owned();
                        self.log(
                            control,
                            Step::Partial {
                                text: logged.clone(),
                            },
                        );
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        if !ticks.is_multiple_of(10) {
            self.log(
                control,
                Step::Audio {
                    ms: u64::from(ticks % 10) * 100,
                },
            );
        }
        lock(&control.route)
            .live
            .retain(|r| r.session != self.session);
        self.log(
            control,
            Step::Final {
                text: (*line).to_owned(),
                sinks: self.sinks.clone(),
            },
        );
    }
}

/// Records a dictation every few seconds: recording with live text, the
/// final result, idle again.
async fn play(control: Arc<FakeControl>) {
    for (n, line) in LINES.iter().cycle().enumerate() {
        tokio::time::sleep(Duration::from_secs(6)).await;
        let pretend = Pretend {
            session: SessionId(format!("example-d{n}")),
            origin: Origin::Dictation { device: None },
            engine: "whisper",
            sinks: vec![Sink::Paste, Sink::Overlay],
        };
        pretend
            .play(&control, line, |text| {
                let mut status = lock(&control.status);
                status.dictation = Dictation::Recording;
                status.live_text = Some(text.to_owned());
            })
            .await;
        {
            let mut status = lock(&control.status);
            status.dictation = Dictation::Idle;
            status.live_text = None;
            status.stats.sessions += 1;
            status.stats.chars += line.chars().count() as u64;
            status.stats.audio_ms += 4_000;
            status.stats.last_chars = line.chars().count() as u64;
            status.stats.last_audio_ms = 4_000;
        }
        lock(&control.activity).push(Entry::Final {
            at: now_ms(),
            source: Source::Dictation,
            text: (*line).to_owned(),
            delivered: Delivered::Pasted,
            session: Some(pretend.session),
            engine: Some("whisper".to_owned()),
        });
    }
}

/// The paired iPad dictates through the gateway now and then, to another
/// engine; one subscribed app is sent its text too.
async fn remote(control: Arc<FakeControl>) {
    tokio::time::sleep(Duration::from_secs(3)).await;
    for (n, line) in REMOTE_LINES.iter().cycle().enumerate() {
        let name = "Kitchen iPad".to_owned();
        let pretend = Pretend {
            session: SessionId(format!("example-r{n}")),
            origin: Origin::Remote {
                client: name.clone(),
            },
            engine: "sherpa",
            sinks: vec![Sink::Client { name }, Sink::Subscribers { count: 1 }],
        };
        pretend.play(&control, line, |_| {}).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// An example prints its link; the library never prints.
#[allow(clippy::print_stderr)]
fn main() {
    let addr: SocketAddr = match std::env::args().nth(1) {
        Some(a) => match a.parse() {
            Ok(addr) => addr,
            Err(e) => {
                eprintln!("{a}: {e}");
                std::process::exit(2);
            }
        },
        None => SocketAddr::from(([127, 0, 0, 1], 0)),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start the runtime: {e}");
            std::process::exit(1);
        }
    };
    runtime.block_on(async {
        let control = Arc::new(FakeControl::sample());
        // Share on the LAN with one app paired, so the Apps section has content.
        {
            let mut config = lock(&control.config);
            config.subscription.lan = true;
            // Each use on a service that needs no key; `cloud` is left without one.
            config
                .recognition
                .set_engine(EngineRole::Dictation, "whisper");
            config
                .recognition
                .set_engine(EngineRole::Continuous, "sherpa");
            config.recognition.set_engine(EngineRole::Remote, "sherpa");
            lock(&control.status).lan = Lan::Listening {
                url: "wss://192.168.1.20:7591/asr".to_owned(),
                fingerprint: "3f:a9:0c:51".to_owned(),
            };
            lock(&control.plugins).extend([
                InstalledPlugin {
                    id: "polish".to_owned(),
                    version: "0.3.0".to_owned(),
                    enabled: true,
                    load: PluginLoad::Ready,
                    title: Some("Polish".into()),
                    description: Some("Tidies punctuation".into()),
                    provides: vec![Provide::Transform, Provide::Action],
                    host: true,
                    permissions: vec!["network:api.openai.com".to_owned()],
                    fields: vec![Field {
                        key: "api_key".to_owned(),
                        label: "API key".into(),
                        help: "Kept in the config file".into(),
                        kind: FieldKind::Secret,
                        required: true,
                        advanced: false,
                    }],
                },
                InstalledPlugin {
                    id: "swap".to_owned(),
                    version: "1.2.0".to_owned(),
                    enabled: false,
                    load: PluginLoad::Off,
                    title: Some("Swap".into()),
                    description: Some("Rewrites a few phrases".into()),
                    provides: vec![Provide::Context, Provide::Transform],
                    host: false,
                    permissions: Vec::new(),
                    fields: Vec::new(),
                },
            ]);
            lock(&control.activity).extend([
                Entry::HookFailed {
                    at: now_ms(),
                    plugin: "polish".to_owned(),
                    hook: Provide::Transform,
                    failure: HookFailure::Timeout,
                    session: None,
                },
                Entry::PluginStalled {
                    at: now_ms(),
                    plugin: "polish".to_owned(),
                },
            ]);
            lock(&control.clients).push(PairedClient {
                id: "c1".to_owned(),
                name: "Kitchen iPad".to_owned(),
                scopes: vec![Scope::Dictate],
                paired_at: 0,
                last_seen: None,
                connections: 1,
            });
        }
        // A few lines debug mode would have recorded.
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            let mut activity = lock(&control.activity);
            activity.push(Entry::Debug {
                at: now - 5_000,
                event: anya_runtime::DebugEvent::KeyDown {
                    combo: "Fn".to_owned(),
                },
            });
            activity.push(Entry::Debug {
                at: now - 4_200,
                event: anya_runtime::DebugEvent::KeyUp {
                    combo: "Fn".to_owned(),
                },
            });
            activity.push(Entry::Debug {
                at: now - 3_700,
                event: anya_runtime::DebugEvent::Timing {
                    connect_ms: Some(210),
                    first_text_ms: Some(830),
                    final_ms: Some(480),
                },
            });
        }
        let server = match web::serve(control.clone(), Arc::new(BrowserShell), addr).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        eprintln!("Anya settings: {}", server.url());
        eprintln!("Tray panel:    {}", server.panel_url());
        tokio::spawn(remote(control.clone()));
        play(control).await;
    });
}
