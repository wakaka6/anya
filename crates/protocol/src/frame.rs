use serde::{Deserialize, Serialize};

/// Identifies one recognition session, unique across restarts of an instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// What opened a session. Subscribers are granted sources, not sessions:
/// dictation reaches an outside app only with the user's grant for that app
/// (docs/decisions/0005-privacy-defaults.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The user holds or toggles a hotkey and talks into the microphone.
    #[default]
    Dictation,
    /// The switched-on transcription of a device, usually a loopback one.
    Continuous,
    /// A source a newer Anya added; skip what it sends.
    #[serde(other)]
    Unknown,
}

/// Whether a source is capturing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// The device is open: speech will be recognized.
    Listening,
    /// Switched off, or capture died.
    Idle,
    /// A state a newer Anya added.
    #[serde(other)]
    Unknown,
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
    /// A reason a newer Anya added; the session is over all the same.
    #[serde(other)]
    Unknown,
}

/// What went wrong in a session that ended with reason `error`. A code, not
/// text: every UI and app shows it in its own language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A required engine setting is empty.
    NotConfigured,
    /// The engine could not be reached.
    Connect,
    /// The engine answered with an error.
    Upstream,
    /// The engine did not send its final result in time.
    Timeout,
    /// Capture failed during the session.
    Audio,
    /// A code a newer Anya added.
    #[serde(other)]
    Unknown,
}

/// A message to subscribers. On the wire: a JSON object whose `type` is the
/// variant name in snake case. Decoding never fails on a frame type, field or
/// value a newer Anya added: they read as `Unknown` or are ignored.
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
        /// Partial results arrive while audio is still being sent. False for
        /// a batch engine: its only result is the final one, after the audio
        /// ends. An older Anya omits it; it reads as false.
        #[serde(default)]
        streaming: bool,
    },
    /// A recognition result.
    Result {
        /// The session it belongs to.
        session: SessionId,
        /// The result, its fields inline beside `type` and `session`.
        #[serde(flatten)]
        recognition: Recognition,
    },
    /// How loud the session's latest audio chunk was, for a level meter. One
    /// per chunk while the session is open (about 25 a second); never replayed
    /// to a late subscriber.
    Level {
        /// The session the audio belongs to.
        session: SessionId,
        /// 0.0 is silence, 1.0 is full scale; perceptually spread.
        level: f32,
    },
    /// The session is over; no more frames name it.
    SessionEnd {
        /// The session that ended.
        session: SessionId,
        /// Why.
        reason: EndReason,
        /// What went wrong, with reason `error`. Apps translate the code.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ErrorCode>,
        /// The engine's or the system's own words, untranslated, for logs
        /// and for showing beside the translated code.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A frame type a newer Anya added; skip it.
    #[serde(other)]
    Unknown,
}
