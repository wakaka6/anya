//! Remote dictation: the sessions a client opens with audio it uploads.
//!
//! A session's audio goes through a task of its own that owns the
//! [`LiveSession`], so a slow engine never stalls the connection that
//! feeds it. Nothing here writes to the host: results leave only as frames
//! on the requesting connection.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anya_core::audio::{CHUNK_BYTES, Chunk};
use anya_core::engine::Choice;
use anya_core::hub::{Canceller, Hub, LiveSession, Outcome};
use anya_protocol::{Refusal, SessionId, Source};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::clients::Clients;

/// How long one recording may run before it stops by itself.
const MAX_RECORDING: Duration = Duration::from_secs(600);
/// Chunks buffered between a connection and its session's task; about ten
/// seconds of audio.
const AUDIO_BUFFER: usize = 256;

/// What the gateway needs on top of the hub: which engine recognizes, who
/// may ask, and how long a final result may take.
#[derive(Clone)]
pub struct Gateway {
    /// The engine and params for the next remote session, read at every
    /// start so an edited setting applies at once.
    pub choice: Choice,
    /// The paired clients.
    pub clients: Clients,
    /// How long a stopped recording waits for its final result.
    pub final_timeout: Duration,
}

/// A session that ended, reported by its task.
pub(crate) struct Ended {
    pub(crate) session: SessionId,
    pub(crate) outcome: Outcome,
}

/// A session that was accepted.
pub(crate) struct Started {
    pub(crate) session: SessionId,
    pub(crate) streaming: bool,
}

/// What became of uploaded audio.
pub(crate) enum Pushed {
    /// No recording is open.
    NoRecording,
    /// Taken.
    Taken,
    /// The session's task fell too far behind; the session was cancelled.
    Overflowed,
}

/// Why a session could not be stopped.
pub(crate) enum StopError {
    /// This connection has no such session.
    Unknown,
    /// It was stopped already.
    Stopped,
}

/// What a finished session leaves for the connection to tell its client.
pub(crate) struct Finished {
    pub(crate) engine: String,
    pub(crate) streaming: bool,
    /// Why the connection itself ended the session, if it did.
    pub(crate) fault: Option<String>,
}

enum Audio {
    Chunk(Chunk),
    /// No more audio, after this last chunk.
    Stop(Option<Chunk>),
}

struct Feed {
    audio: mpsc::Sender<Audio>,
    /// Bytes received that do not fill a chunk yet.
    pending: Vec<u8>,
    deadline: Instant,
}

struct Open {
    canceller: Canceller,
    /// Present while the session takes audio.
    feed: Option<Feed>,
    engine: String,
    streaming: bool,
    fault: Option<String>,
}

/// One connection's remote sessions. Dropping it cancels the unfinished ones.
pub(crate) struct Dictations {
    open: HashMap<SessionId, Open>,
    recording: Option<SessionId>,
    ended: mpsc::UnboundedSender<Ended>,
}

impl Dictations {
    /// Reports ended sessions to `ended`.
    pub(crate) fn new(ended: mpsc::UnboundedSender<Ended>) -> Dictations {
        Dictations {
            open: HashMap::new(),
            recording: None,
            ended,
        }
    }

    /// Opens a session. Must be called inside a Tokio runtime.
    pub(crate) fn start(
        &mut self,
        hub: &Hub,
        gateway: &Gateway,
        language: Option<String>,
        hotwords: Vec<String>,
    ) -> Result<Started, (Refusal, Option<String>)> {
        if self.recording.is_some() {
            return Err((Refusal::Busy, None));
        }
        let not_configured = |e: anya_core::engine::EngineError| {
            (Refusal::NotConfigured, Some(e.detail().to_owned()))
        };
        let (engine, mut params) = (gateway.choice)().map_err(not_configured)?;
        engine.check(&params.settings).map_err(not_configured)?;
        if let Some(language) = language.filter(|l| !l.trim().is_empty()) {
            params.language = Some(language);
        }
        params.hotwords.extend(hotwords);
        let manifest = engine.manifest();
        let (engine_id, streaming) = (manifest.id.clone(), manifest.streaming);
        let live = hub.open(engine.as_ref(), Source::Dictation, params);
        let session = live.id().clone();
        let (audio, queued) = mpsc::channel(AUDIO_BUFFER);
        self.open.insert(
            session.clone(),
            Open {
                canceller: live.canceller(),
                feed: Some(Feed {
                    audio,
                    pending: Vec::new(),
                    deadline: Instant::now() + MAX_RECORDING,
                }),
                engine: engine_id,
                streaming,
                fault: None,
            },
        );
        self.recording = Some(session.clone());
        tokio::spawn(pump(
            live,
            queued,
            gateway.final_timeout,
            self.ended.clone(),
        ));
        Ok(Started { session, streaming })
    }

    /// The session taking audio, if any.
    pub(crate) fn recording(&self) -> Option<&SessionId> {
        self.recording.as_ref()
    }

    /// When the open recording must stop by itself.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let session = self.recording.as_ref()?;
        self.open.get(session)?.feed.as_ref().map(|f| f.deadline)
    }

    /// Takes uploaded bytes: whole chunks go to the session, an odd byte or
    /// a partial chunk waits for the next message.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Pushed {
        let Some(session) = self.recording.clone() else {
            return Pushed::NoRecording;
        };
        let Some(feed) = self.open.get_mut(&session).and_then(|o| o.feed.as_mut()) else {
            return Pushed::NoRecording;
        };
        feed.pending.extend_from_slice(bytes);
        let mut overflowed = false;
        while feed.pending.len() >= CHUNK_BYTES {
            let chunk = Chunk(feed.pending.drain(..CHUNK_BYTES).collect());
            match feed.audio.try_send(Audio::Chunk(chunk)) {
                Ok(()) => {}
                // The session is over; its end is on the way.
                Err(TrySendError::Closed(_)) => break,
                Err(TrySendError::Full(_)) => {
                    overflowed = true;
                    break;
                }
            }
        }
        if overflowed {
            self.abort(
                &session,
                "the audio arrived faster than it could be recognized",
            );
            return Pushed::Overflowed;
        }
        Pushed::Taken
    }

    /// No more audio for `session`: the rest, padded with silence to a
    /// whole chunk, goes in and the engine is asked for its final result.
    pub(crate) fn stop(&mut self, session: &SessionId) -> Result<(), StopError> {
        let open = self.open.get_mut(session).ok_or(StopError::Unknown)?;
        let mut feed = open.feed.take().ok_or(StopError::Stopped)?;
        if self.recording.as_ref() == Some(session) {
            self.recording = None;
        }
        let last = (!feed.pending.is_empty()).then(|| {
            feed.pending.resize(CHUNK_BYTES, 0);
            Chunk(std::mem::take(&mut feed.pending))
        });
        if let Err(TrySendError::Full(_)) = feed.audio.try_send(Audio::Stop(last)) {
            self.abort(
                session,
                "the audio arrived faster than it could be recognized",
            );
        }
        Ok(())
    }

    /// Throws `session` away. False when this connection has no such session.
    pub(crate) fn cancel(&mut self, session: &SessionId) -> bool {
        let Some(open) = self.open.get_mut(session) else {
            return false;
        };
        open.canceller.cancel();
        open.feed = None;
        if self.recording.as_ref() == Some(session) {
            self.recording = None;
        }
        true
    }

    /// Why the connection itself ended `session`, if it did.
    pub(crate) fn fault(&self, session: &SessionId) -> Option<&str> {
        self.open.get(session)?.fault.as_deref()
    }

    /// Forgets a session that ended.
    pub(crate) fn finished(&mut self, session: &SessionId) -> Option<Finished> {
        if self.recording.as_ref() == Some(session) {
            self.recording = None;
        }
        self.open.remove(session).map(|o| Finished {
            engine: o.engine,
            streaming: o.streaming,
            fault: o.fault,
        })
    }

    fn abort(&mut self, session: &SessionId, fault: &str) {
        if let Some(open) = self.open.get_mut(session) {
            open.fault = Some(fault.to_owned());
        }
        self.cancel(session);
    }
}

impl Drop for Dictations {
    fn drop(&mut self) {
        for open in self.open.values() {
            open.canceller.cancel();
        }
    }
}

/// Feeds a session its audio until it ends, then reports how it ended.
async fn pump(
    mut live: LiveSession,
    mut audio: mpsc::Receiver<Audio>,
    final_timeout: Duration,
    ended: mpsc::UnboundedSender<Ended>,
) {
    let session = live.id().clone();
    let outcome = loop {
        match audio.recv().await {
            Some(Audio::Chunk(chunk)) => {
                if !live.send(chunk).await {
                    break live.ended().await;
                }
            }
            Some(Audio::Stop(last)) => {
                if let Some(chunk) = last
                    && !live.send(chunk).await
                {
                    break live.ended().await;
                }
                break live.finish(final_timeout).await;
            }
            // Cancelled or disconnected: the cancel is already queued.
            None => break live.ended().await,
        }
    };
    let _ = ended.send(Ended { session, outcome });
}
