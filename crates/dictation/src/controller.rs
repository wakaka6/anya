//! The task behind [`crate::Dictation`]: one recording at a time, and any
//! number of stopped sessions finishing in the background.
//!
//! The controller owns the capture and reads its chunks itself, so stopping
//! can release the device before anything else happens. Each session gets a
//! pump task that owns its [`LiveSession`]: audio reaches the engine there,
//! so an engine that is behind never holds up the next input.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anya_core::audio::{AudioError, AudioSource, Capture, Chunk};
use anya_core::engine::{Choice, EngineError};
use anya_core::hub::{Canceller, Hub, LiveSession, Outcome};
use anya_protocol::{EndReason, ErrorCode, SessionId, Source, SourceState};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::{ERROR_HOLD, Finished, Input, Mode, Settings, State, Status};

/// What a [`crate::Dictation`] handle sends.
#[derive(Debug)]
pub(crate) enum Command {
    Input(Input),
    Configure(Settings),
}

/// What the controller sends a session's pump.
enum Feed {
    Audio(Chunk),
    /// No more audio: wait this long for the final result.
    Finish(Duration),
}

/// A pump's report: its session is over.
struct Ended {
    session: SessionId,
    outcome: Outcome,
}

/// A session's pump, as the controller holds it.
struct Run {
    session: SessionId,
    feed: mpsc::UnboundedSender<Feed>,
    canceller: Canceller,
    task: JoinHandle<()>,
}

impl Run {
    /// Ends the session `cancelled`. Aborting the pump drops the session
    /// even while it waits on an engine that is behind; the queued cancel
    /// still decides the reason.
    fn cancel(self) {
        self.canceller.cancel();
        self.task.abort();
    }
}

/// The recording in progress.
struct Recording {
    capture: Capture,
    run: Run,
    /// When the stop delay ends; `None` while recording goes on.
    stop_at: Option<Instant>,
}

struct Failure {
    error: ErrorCode,
    detail: Option<String>,
    until: Instant,
}

enum Wake {
    Command(Command),
    Ended(Ended),
    Chunk(Option<Result<Chunk, AudioError>>),
    StopDelay,
    FailureExpired,
}

struct Controller {
    hub: Hub,
    audio: Arc<dyn AudioSource>,
    choice: Choice,
    settings: Settings,
    status: watch::Sender<Status>,
    finished: mpsc::UnboundedSender<Finished>,
    ended: mpsc::UnboundedSender<Ended>,
    /// Dictation hotkeys that are down.
    held: HashSet<u32>,
    recording: Option<Recording>,
    /// Stopped sessions waiting for their final result.
    finishing: HashMap<SessionId, Run>,
    failure: Option<Failure>,
}

pub(crate) fn spawn(
    hub: Hub,
    audio: Arc<dyn AudioSource>,
    choice: Choice,
    settings: Settings,
) -> (
    mpsc::UnboundedSender<Command>,
    watch::Receiver<Status>,
    mpsc::UnboundedReceiver<Finished>,
) {
    let (commands, commands_rx) = mpsc::unbounded_channel();
    let (status_tx, status) = watch::channel(Status::default());
    let (finished_tx, finished) = mpsc::unbounded_channel();
    let (ended_tx, ended_rx) = mpsc::unbounded_channel();
    hub.set_source(Source::Dictation, SourceState::Idle, None);
    let controller = Controller {
        hub,
        audio,
        choice,
        settings,
        status: status_tx,
        finished: finished_tx,
        ended: ended_tx,
        held: HashSet::new(),
        recording: None,
        finishing: HashMap::new(),
        failure: None,
    };
    tokio::spawn(controller.run(commands_rx, ended_rx));
    (commands, status, finished)
}

impl Controller {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut ended: mpsc::UnboundedReceiver<Ended>,
    ) {
        loop {
            let stop_at = self.recording.as_ref().and_then(|r| r.stop_at);
            let fail_until = self.failure.as_ref().map(|f| f.until);
            // Inputs first: they are handled in the order the user gave them.
            let wake = tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(command) => Wake::Command(command),
                    None => break,
                },
                Some(ended) = ended.recv() => Wake::Ended(ended),
                chunk = next_chunk(&mut self.recording) => Wake::Chunk(chunk),
                () = sleep_until(stop_at) => Wake::StopDelay,
                () = sleep_until(fail_until) => Wake::FailureExpired,
            };
            match wake {
                Wake::Command(Command::Input(input)) => self.input(input),
                Wake::Command(Command::Configure(settings)) => {
                    if settings.mode != self.settings.mode {
                        self.held.clear();
                    }
                    self.settings = settings;
                }
                Wake::Ended(ended) => self.ended(ended),
                Wake::Chunk(Some(Ok(chunk))) => {
                    if let Some(r) = &self.recording {
                        let _ = r.run.feed.send(Feed::Audio(chunk));
                    }
                }
                Wake::Chunk(Some(Err(e))) => self.capture_failed(e.to_string()),
                Wake::Chunk(None) => self.capture_failed("capture stopped".to_owned()),
                Wake::StopDelay => self.stop(),
                Wake::FailureExpired => self.failure = None,
            }
            self.publish();
        }
        // Every handle is gone: abandon the recording. Sessions still
        // finishing end by themselves; nobody reads their text any more.
        self.release();
    }

    fn input(&mut self, input: Input) {
        match (self.settings.mode, input) {
            (_, Input::Cancel) => self.cancel(),
            (_, Input::Retry) => {
                if self.recording.is_none() && self.failure.is_some() {
                    self.start();
                }
            }
            (_, Input::Press(key)) if !self.held.insert(key) => {
                // Key repeat: the key is already down.
            }
            (Mode::Toggle, Input::Press(_)) => match &self.recording {
                None => self.start(),
                Some(r) if r.stop_at.is_some() => {
                    self.stop();
                    self.start();
                }
                Some(_) => self.begin_stop(),
            },
            (Mode::Toggle, Input::Release(key)) => {
                self.held.remove(&key);
            }
            (Mode::Hold, Input::Press(_)) => match &mut self.recording {
                None => self.start(),
                // Resumes a recording in its stop delay.
                Some(r) => r.stop_at = None,
            },
            (Mode::Hold, Input::Release(key)) => {
                if self.held.remove(&key) && self.held.is_empty() {
                    self.begin_stop();
                }
            }
        }
    }

    /// Opens the engine's session and the device, in that order of checks:
    /// an engine that cannot open never has the microphone opened for it.
    fn start(&mut self) {
        self.failure = None;
        let (engine, params) = match (self.choice)() {
            Ok(choice) => choice,
            Err(e) => {
                self.fail_engine(&e);
                return;
            }
        };
        if let Err(e) = engine.check(&params.settings) {
            self.fail_engine(&e);
            return;
        }
        let device = self.settings.device.clone();
        let capture = match self.audio.open(device.as_deref()) {
            Ok(capture) => capture,
            Err(e) => {
                self.fail(ErrorCode::Audio, Some(e.to_string()));
                return;
            }
        };
        self.hub
            .set_source(Source::Dictation, SourceState::Listening, device);
        let live = self.hub.open(engine.as_ref(), Source::Dictation, params);
        let session = live.id().clone();
        let canceller = live.canceller();
        let (feed, feed_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(pump(live, feed_rx, self.ended.clone()));
        tracing::debug!(session = session.0.as_str(), "dictation recording");
        self.recording = Some(Recording {
            capture,
            run: Run {
                session,
                feed,
                canceller,
                task,
            },
            stop_at: None,
        });
    }

    /// The user asked to stop: after the stop delay, or now without one.
    fn begin_stop(&mut self) {
        if self.settings.stop_delay.is_zero() {
            self.stop();
        } else if let Some(r) = &mut self.recording {
            r.stop_at = Some(Instant::now() + self.settings.stop_delay);
        }
    }

    /// Releases the device, then lets the session finish in the background.
    fn stop(&mut self) {
        let Some(Recording {
            mut capture, run, ..
        }) = self.recording.take()
        else {
            return;
        };
        // What the device already captured still belongs to the recording.
        capture.chunks.close();
        while let Ok(item) = capture.chunks.try_recv() {
            if let Ok(chunk) = item {
                let _ = run.feed.send(Feed::Audio(chunk));
            }
        }
        drop(capture);
        self.hub
            .set_source(Source::Dictation, SourceState::Idle, None);
        let _ = run.feed.send(Feed::Finish(self.settings.final_timeout));
        tracing::debug!(session = run.session.0.as_str(), "dictation stopped");
        self.finishing.insert(run.session.clone(), run);
    }

    /// Ends the recording without finishing it. Dropping the returned run
    /// abandons the session.
    fn release(&mut self) -> Option<Run> {
        let recording = self.recording.take()?;
        drop(recording.capture);
        self.hub
            .set_source(Source::Dictation, SourceState::Idle, None);
        Some(recording.run)
    }

    fn cancel(&mut self) {
        if let Some(run) = self.release() {
            tracing::debug!(session = run.session.0.as_str(), "dictation cancelled");
            run.cancel();
        }
        for (_, run) in self.finishing.drain() {
            tracing::debug!(
                session = run.session.0.as_str(),
                "dictation result cancelled"
            );
            run.cancel();
        }
        self.failure = None;
    }

    fn capture_failed(&mut self, detail: String) {
        let stopping = self.recording.as_ref().is_some_and(|r| r.stop_at.is_some());
        if stopping {
            // The user already asked to stop: keep what was recorded.
            self.stop();
        } else if self.release().is_none() {
            return;
        }
        self.fail(ErrorCode::Audio, Some(detail));
    }

    fn ended(&mut self, Ended { session, outcome }: Ended) {
        if self.finishing.remove(&session).is_some() {
            self.deliver(session, &outcome.text);
            if outcome.reason == EndReason::Error {
                self.fail(outcome.error.unwrap_or(ErrorCode::Unknown), outcome.detail);
            }
            return;
        }
        let current = self
            .recording
            .as_ref()
            .is_some_and(|r| r.run.session == session);
        if !current {
            return;
        }
        // The session ended while the user was still recording.
        self.release();
        match outcome.reason {
            // The engine closed the session with its final text itself.
            EndReason::Final => self.deliver(session, &outcome.text),
            EndReason::Error => {
                self.fail(outcome.error.unwrap_or(ErrorCode::Unknown), outcome.detail)
            }
            EndReason::Cancelled => {}
            EndReason::Closed | EndReason::Unknown => self.fail(ErrorCode::Connect, None),
        }
    }

    fn deliver(&self, session: SessionId, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let _ = self.finished.send(Finished {
            session,
            text: text.to_owned(),
        });
    }

    fn fail_engine(&mut self, e: &EngineError) {
        self.fail(e.code(), Some(e.detail().to_owned()));
    }

    fn fail(&mut self, error: ErrorCode, detail: Option<String>) {
        tracing::warn!(?error, ?detail, "dictation failed");
        self.failure = Some(Failure {
            error,
            detail,
            until: Instant::now() + ERROR_HOLD,
        });
    }

    fn publish(&self) {
        let state = match (&self.recording, &self.failure) {
            (Some(r), _) if r.stop_at.is_some() => State::Stopping,
            (Some(_), _) => State::Recording,
            (None, Some(f)) => State::Failed {
                error: f.error,
                detail: f.detail.clone(),
            },
            (None, None) => State::Idle,
        };
        let status = Status {
            state,
            finishing: self.finishing.len(),
        };
        self.status.send_if_modified(|current| {
            if *current == status {
                return false;
            }
            *current = status;
            true
        });
    }
}

async fn next_chunk(recording: &mut Option<Recording>) -> Option<Result<Chunk, AudioError>> {
    match recording {
        Some(r) => r.capture.chunks.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Feeds one session until it is finished, abandoned or over by itself, and
/// reports how it ended. A closed feed abandons the session.
async fn pump(
    mut live: LiveSession,
    mut feed: mpsc::UnboundedReceiver<Feed>,
    ended: mpsc::UnboundedSender<Ended>,
) {
    let session = live.id().clone();
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = live.ended() => break outcome,
            item = feed.recv() => match item {
                Some(Feed::Audio(chunk)) => {
                    live.send(chunk).await;
                }
                Some(Feed::Finish(timeout)) => break live.finish(timeout).await,
                None => return,
            },
        }
    };
    let _ = ended.send(Ended { session, outcome });
}
