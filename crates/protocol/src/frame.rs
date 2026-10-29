use serde::{Deserialize, Serialize};

/// Identifies one recognition session, unique across restarts of an instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// What opened a session. Subscribers are granted sources, not sessions:
/// dictation reaches an outside app only with the user's grant for that app
/// (docs/decisions/0005-privacy-defaults.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The user holds or toggles a hotkey and talks into the microphone.
    Dictation,
    /// The switched-on transcription of a device, usually a loopback one.
    Continuous,
}

/// Whether a source is capturing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// The device is open: speech will be recognized.
    Listening,
    /// Switched off, or capture died.
    Idle,
}

/// One sentence as the engine split it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utterance {
    /// The sentence's text.
    pub text: String,
    /// The engine will not change this sentence any more.
    pub definite: bool,
}

/// One recognition result, as an engine reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recognition {
    /// The session's whole text so far, not a delta.
    pub text: String,
    /// The engine's sentences; empty for engines that do not split.
    #[serde(default)]
    pub utterances: Vec<Utterance>,
    /// True only on the engine's last result for the session.
    #[serde(rename = "final")]
    pub is_final: bool,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// The engine sent its final result.
    Final,
    /// Recognition or sending audio failed.
    Error,
    /// The connection closed, capture stopped, or Anya shut down.
    Closed,
    /// The user cancelled; whatever was recognized is not output.
    Cancelled,
}

/// A message to subscribers. On the wire: a JSON object whose `type` is the
/// variant name in snake case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// The first frame on every connection.
    Hello {
        /// Always `anya`.
        name: String,
        /// Anya's version.
        version: String,
        /// [`crate::PROTOCOL_VERSION`] of this instance.
        protocol: u32,
    },
    /// A source started or stopped capturing. Also sent once per granted
    /// source right after `hello`, so a quiet stream and a switched-off
    /// source can be told apart.
    Source {
        /// The source this is about.
        source: Source,
        /// Its state now.
        state: SourceState,
        /// The device it captures from, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    /// The engine connected; results for `session` follow.
    SessionStart {
        /// The new session.
        session: SessionId,
        /// What opened it.
        source: Source,
        /// The engine's manifest id.
        engine: String,
    },
    /// A recognition result.
    Result {
        /// The session it belongs to.
        session: SessionId,
        /// The result, its fields inline beside `type` and `session`.
        #[serde(flatten)]
        recognition: Recognition,
    },
    /// The session is over; no more frames name it.
    SessionEnd {
        /// The session that ended.
        session: SessionId,
        /// Why.
        reason: EndReason,
    },
}
