//! Continuous transcription: a device kept open, cut into sessions by a silence gate.
//!
//! [`spawn`] starts one controller task and returns the [`Continuous`] switch.
//! While switched on, the controller keeps the chosen device open and publishes
//! the `continuous` source as listening; it reopens the device only when
//! capture itself fails. With the silence gate on, speech opens a session and
//! silence finishes it, so a quiet room uploads nothing. With the gate off,
//! one long session streams everything and reconnects with backoff, replaying
//! the last few seconds into the new session.

mod gate;
mod sessions;

use std::sync::Arc;
use std::time::Duration;

use anya_core::audio::{AudioError, AudioSource, Capture, DeviceKind};
use anya_core::engine::Choice;
use anya_core::hub::Hub;
use anya_protocol::{Source, SourceState};
use tokio::sync::watch;

use crate::gate::Feed;

/// First wait before reopening a device or reconnecting an engine.
const BACKOFF_MIN: Duration = Duration::from_millis(500);
/// The wait doubles per failure up to this.
const BACKOFF_MAX: Duration = Duration::from_secs(5);

fn grow(backoff: Duration) -> Duration {
    (backoff * 2).min(BACKOFF_MAX)
}

/// How continuous transcription runs while it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The device to capture, an id or a name from
    /// [`AudioSource::devices`]; `None` for the system sound default.
    pub device: Option<String>,
    /// Speech opens a session and silence finishes it. Off, one long session
    /// streams everything and reconnects when it ends.
    pub silence_gate: bool,
    /// How long the engine gets to send its final result after the last audio.
    pub final_timeout: Duration,
}

impl Default for Settings {
    /// The system sound default, the gate on, two seconds for the final result.
    fn default() -> Settings {
        Settings {
            device: None,
            silence_gate: true,
            final_timeout: Duration::from_secs(2),
        }
    }
}

/// The switch of a running continuous transcription. Cheap to clone; clones
/// share one controller task, which stops once every clone is dropped.
#[derive(Debug, Clone)]
pub struct Continuous {
    desired: Arc<watch::Sender<Option<Settings>>>,
}

impl Continuous {
    /// Switches transcription on with `settings`, or off. Returns at once:
    /// opening and releasing the device, and finishing a session, run on the
    /// controller task, one change after another, so quick calls collapse
    /// into the last. While on, identical settings change nothing, another
    /// device reopens capture, and any other change applies to the next
    /// session without interrupting the current one.
    pub fn set(&self, on: bool, settings: Settings) {
        let want = on.then_some(settings);
        self.desired.send_if_modified(|now| {
            if *now == want {
                return false;
            }
            *now = want;
            true
        });
    }
}

/// What the controller reads from the app.
struct Shared {
    hub: Hub,
    audio: Arc<dyn AudioSource>,
    choice: Choice,
    params: watch::Receiver<u64>,
}

/// Starts continuous transcription, switched off. Sessions open on the engine
/// `choice` gives at each open; a bump of `params_changed` (the hotwords
/// changed) reconnects a long session once so it takes them. Must be called
/// inside a Tokio runtime.
pub fn spawn(
    hub: Hub,
    audio: Arc<dyn AudioSource>,
    choice: Choice,
    params_changed: watch::Receiver<u64>,
) -> Continuous {
    hub.set_source(Source::Continuous, SourceState::Idle, None);
    let (desired, rx) = watch::channel(None);
    let shared = Shared {
        hub,
        audio,
        choice,
        params: params_changed,
    };
    tokio::spawn(control(shared, rx));
    Continuous {
        desired: Arc::new(desired),
    }
}

/// Why a capture stopped.
enum Stop {
    /// Switched off, or to another device.
    Switched,
    /// Capture failed; reopen after a backoff.
    Failed,
    /// Every [`Continuous`] was dropped.
    Closed,
}

/// The controller: one change at a time, so teardowns never overlap.
async fn control(shared: Shared, mut desired: watch::Receiver<Option<Settings>>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        if desired.has_changed().is_err() {
            return;
        }
        let Some(settings) = desired.borrow_and_update().clone() else {
            backoff = BACKOFF_MIN;
            if desired.changed().await.is_err() {
                return;
            }
            continue;
        };
        let device = settings.device.clone();
        let (opened, name) = open(&shared, device.clone()).await;
        // Switched off or to another device while opening: drop what opened
        // and follow the latest wish.
        let current = desired.borrow_and_update().clone();
        let Some(settings) = current.filter(|s| s.device == device) else {
            continue;
        };
        match opened {
            Err(e) => {
                tracing::warn!(error = %e, ?backoff, "continuous: cannot open the device");
                shared
                    .hub
                    .set_source(Source::Continuous, SourceState::Idle, name);
                if !pause(&mut desired, backoff).await {
                    return;
                }
                backoff = grow(backoff);
            }
            Ok(capture) => {
                backoff = BACKOFF_MIN;
                tracing::info!(device = ?name, gate = settings.silence_gate, "continuous: listening");
                shared
                    .hub
                    .set_source(Source::Continuous, SourceState::Listening, name.clone());
                match run(&shared, capture, name, settings, &mut desired).await {
                    Stop::Switched => tracing::info!("continuous: stopped"),
                    Stop::Closed => return,
                    Stop::Failed => {
                        tracing::warn!(?backoff, "continuous: capture stopped, reopening");
                        if !pause(&mut desired, backoff).await {
                            return;
                        }
                        backoff = grow(backoff);
                    }
                }
            }
        }
    }
}

/// Opens `device` off the runtime, with the name to publish for it.
async fn open(
    shared: &Shared,
    device: Option<String>,
) -> (Result<Capture, AudioError>, Option<String>) {
    let audio = Arc::clone(&shared.audio);
    let fallback = device.clone();
    let task = tokio::task::spawn_blocking(move || {
        let opened = audio.open(device.as_deref(), DeviceKind::Loopback);
        let name = device.map(|d| display_name(audio.as_ref(), d));
        (opened, name)
    });
    match task.await {
        Ok(done) => done,
        Err(e) => (
            Err(AudioError::Backend(format!(
                "opening the device failed: {e}"
            ))),
            fallback,
        ),
    }
}

/// The name of the device `wanted` refers to, by id or by name; `wanted`
/// itself when the device list does not have it.
fn display_name(audio: &dyn AudioSource, wanted: String) -> String {
    audio
        .devices()
        .ok()
        .and_then(|devices| {
            devices
                .into_iter()
                .find(|d| d.id == wanted || d.name == wanted)
                .map(|d| d.name)
        })
        .unwrap_or(wanted)
}

/// Waits `backoff`, or less when the wish changes. False once every switch is gone.
async fn pause(desired: &mut watch::Receiver<Option<Settings>>, backoff: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(backoff) => true,
        changed = desired.changed() => changed.is_ok(),
    }
}

/// Runs one open capture: the pump feeds the gate, the session loop reads it.
/// Returns once capture failed, or after the device was released and the
/// current session finished because the wish changed.
async fn run(
    shared: &Shared,
    capture: Capture,
    device: Option<String>,
    settings: Settings,
    desired: &mut watch::Receiver<Option<Settings>>,
) -> Stop {
    let (live_tx, live) = watch::channel(settings);
    let (stop_tx, stop_rx) = watch::channel(false);
    let feed = Feed::default();
    let pump_live = live.clone();
    let work = async {
        let (failed, ()) = tokio::join!(
            pump(shared, capture, device, &feed, pump_live, stop_rx),
            sessions::run(shared, &feed, live),
        );
        failed
    };
    tokio::pin!(work);
    let reason = tokio::select! {
        biased;
        failed = &mut work => return if failed { Stop::Failed } else { Stop::Switched },
        reason = follow(desired, &live_tx) => reason,
    };
    let _ = stop_tx.send(true);
    work.await;
    reason
}

/// Passes changed settings to the running capture until the device changes,
/// transcription is switched off or every switch is dropped.
async fn follow(
    desired: &mut watch::Receiver<Option<Settings>>,
    live: &watch::Sender<Settings>,
) -> Stop {
    loop {
        if desired.changed().await.is_err() {
            return Stop::Closed;
        }
        let Some(next) = desired.borrow_and_update().clone() else {
            return Stop::Switched;
        };
        let same_device = live.borrow().device == next.device;
        if !same_device {
            return Stop::Switched;
        }
        live.send_if_modified(|now| {
            if *now == next {
                return false;
            }
            *now = next;
            true
        });
    }
}

/// Drains capture into the gate until told to stop or capture fails, then
/// releases the device and says so. Never waits on an engine. Returns whether
/// capture failed.
async fn pump(
    shared: &Shared,
    mut capture: Capture,
    device: Option<String>,
    feed: &Feed,
    live: watch::Receiver<Settings>,
    mut stop: watch::Receiver<bool>,
) -> bool {
    let failed = loop {
        tokio::select! {
            biased;
            () = stopped(&mut stop) => break false,
            chunk = capture.chunks.recv() => match chunk {
                Some(Ok(chunk)) => {
                    let gated = live.borrow().silence_gate;
                    feed.push(gated, chunk);
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "continuous: capture failed");
                    break true;
                }
                None => {
                    tracing::warn!("continuous: capture ended");
                    break true;
                }
            },
        }
    };
    drop(capture);
    shared
        .hub
        .set_source(Source::Continuous, SourceState::Idle, device);
    feed.close();
    failed
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}
