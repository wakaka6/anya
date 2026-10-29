//! The session hub: opens sessions on engines, turns what they report into
//! [`Frame`]s and keeps the promises in docs/protocol.md for every reader,
//! built-in sink or subscriber.
//!
//! Triggers own capture and decide when a session opens and ends; the hub
//! never opens a device. A trigger calls [`Hub::set_source`] when its device
//! opens or closes, [`Hub::open`] when it wants recognition, and feeds the
//! [`LiveSession`] it gets back.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anya_protocol::{
    EndReason, ErrorCode, Frame, PROTOCOL_VERSION, SessionId, Source, SourceState,
};
use tokio::sync::{broadcast, mpsc, watch};

use crate::audio::Chunk;
use crate::engine::{Engine, Input, Output, SessionParams};

/// Frames a reader may fall behind before it misses some; the server
/// disconnects a subscriber that does (docs/protocol.md, promise 10).
pub const BUFFER: usize = 256;

/// What the hub says about itself in `hello` and session ids.
#[derive(Debug, Clone)]
pub struct HubInfo {
    /// Anya's version, for `hello`.
    pub version: String,
    /// Starts every session id, so ids stay unique across restarts:
    /// `anya-20261030T101500` gives `anya-20261030T101500-1`, `-2`…
    pub session_prefix: String,
}

/// How a session ended, for the trigger that opened it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The reason in its `session_end`.
    pub reason: EndReason,
    /// The session's last text: the final result's with reason `final`,
    /// otherwise whatever was recognized before it ended.
    pub text: String,
    /// What went wrong, with reason `error`.
    pub error: Option<ErrorCode>,
    /// The untranslated specifics, with reason `error`.
    pub detail: Option<String>,
}

/// A reader's start: what to read first, then the live frames.
#[derive(Debug)]
pub struct Subscription {
    /// `hello`, every source's state, then each running session's
    /// `session_start` and latest `result` (promise 9).
    pub snapshot: Vec<Frame>,
    /// Every frame published after the snapshot was taken, none missing.
    pub frames: broadcast::Receiver<Frame>,
}

/// Publishes sessions and source states. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

struct Inner {
    info: HubInfo,
    next: AtomicU64,
    tx: broadcast::Sender<Frame>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    sources: Vec<(Source, SourceState, Option<String>)>,
    running: Vec<Running>,
}

struct Running {
    start: Frame,
    latest: Option<Frame>,
}

impl Running {
    fn id(&self) -> Option<&SessionId> {
        match &self.start {
            Frame::SessionStart { session, .. } => Some(session),
            _ => None,
        }
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-publish; the state is still
        // consistent enough to keep publishing.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Records `frame` for late readers and sends it, under the lock that
    /// [`Hub::subscribe`] takes, so a reader gets each frame exactly once.
    fn publish(&self, frame: Frame) {
        let mut state = self.state();
        match &frame {
            Frame::SessionStart { .. } => state.running.push(Running {
                start: frame.clone(),
                latest: None,
            }),
            Frame::Result { session, .. } => {
                if let Some(r) = state.running.iter_mut().find(|r| r.id() == Some(session)) {
                    r.latest = Some(frame.clone());
                }
            }
            Frame::SessionEnd { session, .. } => state.running.retain(|r| r.id() != Some(session)),
            _ => {}
        }
        let _ = self.tx.send(frame);
    }

    fn is_running(&self, session: &SessionId) -> bool {
        self.state().running.iter().any(|r| r.id() == Some(session))
    }
}

impl Hub {
    /// A hub with no sources and no sessions.
    pub fn new(info: HubInfo) -> Hub {
        let (tx, _) = broadcast::channel(BUFFER);
        Hub {
            inner: Arc::new(Inner {
                info,
                next: AtomicU64::new(1),
                tx,
                state: Mutex::default(),
            }),
        }
    }

    /// Starts reading: the snapshot, then every later frame.
    pub fn subscribe(&self) -> Subscription {
        let state = self.inner.state();
        let mut snapshot = vec![Frame::Hello {
            name: "anya".to_owned(),
            version: self.inner.info.version.clone(),
            protocol: PROTOCOL_VERSION,
        }];
        snapshot.extend(
            state
                .sources
                .iter()
                .map(|(source, s, device)| Frame::Source {
                    source: *source,
                    state: *s,
                    device: device.clone(),
                }),
        );
        for r in &state.running {
            snapshot.push(r.start.clone());
            snapshot.extend(r.latest.clone());
        }
        Subscription {
            snapshot,
            frames: self.inner.tx.subscribe(),
        }
    }

    /// Says whether `source` is capturing, and from which device. Publishes a
    /// `source` frame only when something changed.
    pub fn set_source(&self, source: Source, state: SourceState, device: Option<String>) {
        {
            let mut st = self.inner.state();
            let now = (source, state, device.clone());
            match st.sources.iter_mut().find(|(s, ..)| *s == source) {
                Some(entry) if *entry == now => return,
                Some(entry) => *entry = now,
                None => st.sources.push(now),
            }
        }
        self.inner.publish(Frame::Source {
            source,
            state,
            device,
        });
    }

    /// The latest state of each source that has one.
    pub fn sources(&self) -> Vec<(Source, SourceState, Option<String>)> {
        self.inner.state().sources.clone()
    }

    /// Opens a session of `source` on `engine`. Must be called inside a Tokio
    /// runtime. `session_start` is published when the engine is ready; an
    /// engine that fails before that publishes nothing and ends the session
    /// with reason `error`.
    pub fn open(&self, engine: &dyn Engine, source: Source, params: SessionParams) -> LiveSession {
        let n = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let id = SessionId(format!("{}-{n}", self.inner.info.session_prefix));
        let engine_id = engine.manifest().id.clone();
        let session = engine.open(params);
        let (control, controls) = mpsc::channel(2);
        let (outcome_tx, outcome) = watch::channel(None);
        tokio::spawn(drive(
            Arc::clone(&self.inner),
            id.clone(),
            source,
            engine_id,
            session.output,
            controls,
            outcome_tx,
        ));
        LiveSession {
            inner: Arc::clone(&self.inner),
            id,
            input: Some(session.input),
            control,
            outcome,
        }
    }
}

enum Control {
    Cancel,
    Timeout,
}

/// One open session, held by the trigger that opened it. Dropping it
/// abandons the session: it ends with reason `closed`.
pub struct LiveSession {
    inner: Arc<Inner>,
    id: SessionId,
    input: Option<mpsc::Sender<Input>>,
    control: mpsc::Sender<Control>,
    outcome: watch::Receiver<Option<Outcome>>,
}

impl LiveSession {
    /// The session's id.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Sends audio, and its `level` once the session has started. Waits while
    /// the engine is behind. Returns false once the session is over.
    pub async fn send(&self, chunk: Chunk) -> bool {
        let Some(input) = &self.input else {
            return false;
        };
        if self.inner.is_running(&self.id) {
            self.inner.publish(Frame::Level {
                session: self.id.clone(),
                level: chunk.level(),
            });
        }
        input.send(Input::Audio(chunk)).await.is_ok()
    }

    /// The outcome, if the session is already over.
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome.borrow().clone()
    }

    /// Resolves when the session ends by itself: the engine failed or closed.
    pub async fn ended(&mut self) -> Outcome {
        wait(&mut self.outcome).await
    }

    /// No more audio: waits up to `timeout` for the engine's final result.
    /// Past it the session ends with reason `error` and [`ErrorCode::Timeout`].
    pub async fn finish(mut self, timeout: Duration) -> Outcome {
        if let Some(input) = self.input.take() {
            let _ = input.send(Input::Finish).await;
        }
        match tokio::time::timeout(timeout, wait(&mut self.outcome)).await {
            Ok(outcome) => outcome,
            Err(_) => {
                let _ = self.control.send(Control::Timeout).await;
                wait(&mut self.outcome).await
            }
        }
    }

    /// The user threw the session away: it ends with reason `cancelled`.
    pub async fn cancel(mut self) -> Outcome {
        let _ = self.control.send(Control::Cancel).await;
        // After Cancel is queued, so the engine closing cannot win the race
        // and end the session as `closed`.
        self.input = None;
        wait(&mut self.outcome).await
    }
}

async fn wait(outcome: &mut watch::Receiver<Option<Outcome>>) -> Outcome {
    match outcome.wait_for(Option::is_some).await {
        Ok(o) => o.clone().unwrap_or_else(closed_outcome),
        Err(_) => closed_outcome(),
    }
}

fn closed_outcome() -> Outcome {
    Outcome {
        reason: EndReason::Closed,
        text: String::new(),
        error: None,
        detail: None,
    }
}

async fn drive(
    inner: Arc<Inner>,
    id: SessionId,
    source: Source,
    engine: String,
    mut output: mpsc::Receiver<Output>,
    mut controls: mpsc::Receiver<Control>,
    outcome: watch::Sender<Option<Outcome>>,
) {
    let mut started = false;
    let mut text = String::new();
    let start = |started: &mut bool| {
        if !*started {
            *started = true;
            inner.publish(Frame::SessionStart {
                session: id.clone(),
                source,
                engine: engine.clone(),
            });
        }
    };
    let (reason, error, detail) = loop {
        tokio::select! {
            biased;
            control = controls.recv() => match control {
                Some(Control::Cancel) => break (EndReason::Cancelled, None, None),
                Some(Control::Timeout) => break (EndReason::Error, Some(ErrorCode::Timeout), None),
                None => break (EndReason::Closed, None, None),
            },
            out = output.recv() => match out {
                Some(Output::Ready) => start(&mut started),
                Some(Output::Result(recognition)) => {
                    start(&mut started);
                    text.clone_from(&recognition.text);
                    let is_final = recognition.is_final;
                    inner.publish(Frame::Result { session: id.clone(), recognition });
                    if is_final {
                        break (EndReason::Final, None, None);
                    }
                }
                Some(Output::Error(e)) => {
                    break (EndReason::Error, Some(e.code()), Some(e.detail().to_owned()))
                }
                None => break (EndReason::Closed, None, None),
            },
        }
    };
    if started {
        inner.publish(Frame::SessionEnd {
            session: id,
            reason,
            error,
            detail: detail.clone(),
        });
    }
    let _ = outcome.send(Some(Outcome {
        reason,
        text,
        error,
        detail,
    }));
}
