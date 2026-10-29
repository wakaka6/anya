//! Recognition: the seam every speech engine plugs into, built-in or plugin.

use std::collections::BTreeMap;

use anya_protocol::{Manifest, Recognition};
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
    /// Connected: results follow.
    Ready,
    /// A recognition result.
    Result(Recognition),
    /// The session failed and is over.
    Error(EngineError),
}

/// Why a session failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EngineError {
    /// A required setting is empty.
    #[error("setting {0:?} is missing")]
    NotConfigured(String),
    /// The engine could not be reached.
    #[error("cannot reach the engine: {0}")]
    Connect(String),
    /// The engine answered with an error.
    #[error("the engine failed: {0}")]
    Upstream(String),
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

    /// Opens a session and returns at once. Connecting happens on a task the
    /// engine spawns on the current Tokio runtime, so this must be called
    /// from inside one; a failure arrives as [`Output::Error`].
    fn open(&self, params: SessionParams) -> Session;
}
