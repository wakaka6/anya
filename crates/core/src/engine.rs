//! Recognition: the seam every speech engine plugs into, built-in or plugin.

use std::collections::BTreeMap;
use std::sync::Arc;

use anya_protocol::{ErrorCode, Manifest, Recognition, Utterance};
use tokio::sync::mpsc;

use crate::audio::Chunk;

/// What a session is given when it opens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionParams {
    /// BCP-47 tag; `None` leaves the language to the engine.
    pub language: Option<String>,
    /// Words to favour, already merged from every place the user keeps them.
    pub hotwords: Vec<String>,
    /// The engine's settings, keyed by its manifest's field keys. Read at
    /// every open, so an edited setting applies to the next session.
    pub settings: BTreeMap<String, String>,
}

impl SessionParams {
    /// A setting's value, trimmed; `None` when unset or blank.
    pub fn setting(&self, key: &str) -> Option<&str> {
        self.settings
            .get(key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }
}

/// Sent into a session.
#[derive(Debug)]
pub enum Input {
    /// More audio.
    Audio(Chunk),
    /// No more audio: the engine sends its final result, then ends.
    Finish,
}

/// Sent out of a session. The channel closes after the final result or an error.
#[derive(Debug)]
pub enum Output {
    /// Connected: results follow. Sent before the first result.
    Ready,
    /// A recognition result.
    Result(Recognition),
    /// The session failed and is over.
    Error(EngineError),
}

/// Why a session failed. UIs show [`EngineError::code`] translated, with
/// [`EngineError::detail`] beside it; the Display text is for logs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// A required setting is empty; carries the field's key.
    #[error("setting {0:?} is missing")]
    NotConfigured(String),
    /// A setting holds a value the engine cannot use; carries the field's
    /// key. On the wire it is still `not_configured`: the fix is the same.
    #[error("setting {0:?} has an invalid value")]
    Invalid(String),
    /// The engine could not be reached.
    #[error("cannot reach the engine: {0}")]
    Connect(String),
    /// The engine answered with an error.
    #[error("the engine failed: {0}")]
    Upstream(String),
}

impl EngineError {
    /// The code a `session_end` carries.
    pub fn code(&self) -> ErrorCode {
        match self {
            EngineError::NotConfigured(_) | EngineError::Invalid(_) => ErrorCode::NotConfigured,
            EngineError::Connect(_) => ErrorCode::Connect,
            EngineError::Upstream(_) => ErrorCode::Upstream,
        }
    }

    /// The untranslated specifics: the missing or invalid setting's key, or
    /// the engine's or the network's own message.
    pub fn detail(&self) -> &str {
        match self {
            EngineError::NotConfigured(s)
            | EngineError::Invalid(s)
            | EngineError::Connect(s)
            | EngineError::Upstream(s) => s,
        }
    }
}

/// An open session's two ends. Dropping `input` without sending
/// [`Input::Finish`] abandons the session: it closes without a final result.
#[derive(Debug)]
pub struct Session {
    /// Audio in.
    pub input: mpsc::Sender<Input>,
    /// Results out.
    pub output: mpsc::Receiver<Output>,
}

/// A speech engine. Adapters: one crate per built-in engine, and the plugin
/// host's adapter for engines that run in Bun.
pub trait Engine: Send + Sync {
    /// What the engine is and which settings it takes.
    fn manifest(&self) -> &Manifest;

    /// Whether `settings` are enough to open a session. The default checks
    /// the manifest's required fields; an engine with rules a field list
    /// cannot state (one of two credentials) overrides it.
    fn check(&self, settings: &BTreeMap<String, String>) -> Result<(), EngineError> {
        let missing = self
            .manifest()
            .fields
            .iter()
            .find(|f| f.required && settings.get(&f.key).is_none_or(|v| v.trim().is_empty()));
        match missing {
            Some(f) => Err(EngineError::NotConfigured(f.key.clone())),
            None => Ok(()),
        }
    }

    /// Opens a session and returns at once. Connecting happens on a task the
    /// engine spawns on the current Tokio runtime, so this must be called
    /// from inside one; a failure arrives as [`Output::Error`].
    fn open(&self, params: SessionParams) -> Session;
}

/// What a trigger opens its next session with: the engine the user chose and
/// its params, read fresh at every call so an edited setting or hotword file
/// applies to the next session. The app builds it from the config.
pub type Choice =
    Arc<dyn Fn() -> Result<(Arc<dyn Engine>, SessionParams), EngineError> + Send + Sync>;

/// Buffered audio a session holds before it is sent; about two minutes.
const INPUT_CAPACITY: usize = 4096;

/// A session for engines that take the whole recording at once: it is ready
/// at once, keeps the audio, and on [`Input::Finish`] hands the PCM to
/// `transcribe` and reports its text as the one, final result. Abandoned
/// sessions never call `transcribe`.
pub fn batch_session<F, Fut>(transcribe: F) -> Session
where
    F: FnOnce(Vec<u8>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<String, EngineError>> + Send + 'static,
{
    let (input, mut audio) = mpsc::channel(INPUT_CAPACITY);
    let (results, output) = mpsc::channel(8);
    tokio::spawn(async move {
        if results.send(Output::Ready).await.is_err() {
            return;
        }
        let mut pcm = Vec::new();
        loop {
            match audio.recv().await {
                Some(Input::Audio(Chunk(bytes))) => pcm.extend_from_slice(&bytes),
                Some(Input::Finish) => break,
                None => return,
            }
        }
        let out = if pcm.is_empty() {
            Ok(String::new())
        } else {
            transcribe(pcm).await
        };
        let _ = results
            .send(match out {
                Ok(text) => Output::Result(final_text(text)),
                Err(e) => Output::Error(e),
            })
            .await;
    });
    Session { input, output }
}

/// The one final result of a batch engine.
pub fn final_text(text: String) -> Recognition {
    let text = text.trim().to_owned();
    let utterances = if text.is_empty() {
        Vec::new()
    } else {
        vec![Utterance {
            text: text.clone(),
            definite: true,
        }]
    };
    Recognition {
        text,
        utterances,
        is_final: true,
    }
}
