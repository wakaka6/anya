//! Serves the settings page and the tray panel against a fake Anya, for
//! trying the page in a browser:
//!
//! ```bash
//! cargo run -p anya-gui --example web            # picks a port
//! cargo run -p anya-gui --example web 127.0.0.1:7600
//! ```
//!
//! It prints the sign-in link; open it, and `/panel.html` for the panel.
//! Every few seconds the fake records a dictation, so the live states show.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anya_gui::{Shell, ShellKind, web};
use anya_protocol::Source;
use anya_runtime::{Delivered, Dictation, Entry};

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

/// Records a dictation every few seconds: recording with live text, the
/// final result, idle again.
async fn play(control: Arc<FakeControl>) {
    for line in LINES.iter().cycle() {
        tokio::time::sleep(Duration::from_secs(6)).await;
        let words: Vec<&str> = line.split_inclusive(' ').collect();
        let parts = if words.len() > 1 {
            words
        } else {
            line.split_inclusive('，').collect()
        };
        let mut live = String::new();
        for part in parts {
            live.push_str(part);
            {
                let mut status = lock(&control.status);
                status.dictation = Dictation::Recording;
                status.live_text = Some(live.trim().to_owned());
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
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
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        lock(&control.activity).push(Entry::Final {
            at,
            source: Source::Dictation,
            text: (*line).to_owned(),
            delivered: Delivered::Pasted,
        });
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
        let server = match web::serve(control.clone(), Arc::new(BrowserShell), addr).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        eprintln!("Anya settings: {}", server.url());
        eprintln!("Tray panel:    {}", server.panel_url());
        play(control).await;
    });
}
