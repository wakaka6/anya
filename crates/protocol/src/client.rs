use serde::{Deserialize, Serialize};

use crate::frame::SessionId;

/// The WebSocket subprotocol every gateway client offers.
pub const SUBPROTOCOL: &str = "anya.v0";

/// A client that cannot set an `Authorization` header (a browser page)
/// offers its token as a second subprotocol, this prefix and the token.
/// Anya answers with [`SUBPROTOCOL`] only, so the token is never echoed.
pub const TOKEN_SUBPROTOCOL_PREFIX: &str = "anya.token.";

/// Audio a client uploads: binary WebSocket messages of signed 16-bit
/// little-endian mono PCM at this rate. Any whole number of samples per
/// message; 40 ms (1280 bytes) keeps level meters smooth.
pub const UPLOAD_SAMPLE_RATE: u32 = 16_000;

/// What a paired client may do. A client holds the scopes the user gave it
/// in the settings; a connection without a token holds none and, on
/// loopback only, still receives continuous transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Start dictation sessions with the client's own microphone audio and
    /// receive their results.
    Dictate,
    /// Receive continuous transcription.
    SubscribeContinuous,
    /// Receive every dictation session, also the host's own and other
    /// clients'.
    SubscribeDictation,
    /// A scope a newer Anya added; grants nothing here.
    #[serde(other)]
    Unknown,
}

/// A JSON text message a client sends. Unknown types read as
/// [`ClientMessage::Unknown`] and are refused with [`Refusal::BadRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Trades a pairing code shown in Anya's settings for a token. Sent on
    /// a connection without a token; answered with [`Reply::Paired`].
    Pair {
        /// The code as shown, digits only.
        code: String,
        /// What the user sees in the list of paired clients.
        name: String,
    },
    /// Starts a dictation session fed by this connection's binary messages.
    /// One recording per connection at a time; answered with
    /// [`Reply::DictationAccepted`] before any frame of the session.
    DictationStart {
        /// The client's own id for this request, echoed in the reply.
        request_id: String,
        /// BCP-47 tag; absent uses the gateway's configured language.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// Words to favour, added to the gateway's own.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hotwords: Vec<String>,
    },
    /// No more audio: the engine sends its final result, then the session ends.
    DictationStop {
        /// The session from [`Reply::DictationAccepted`].
        session: SessionId,
    },
    /// Throws the recording away: it ends with reason `cancelled`.
    DictationCancel {
        /// The session from [`Reply::DictationAccepted`].
        session: SessionId,
    },
    /// A message type a newer client sent.
    #[serde(other)]
    Unknown,
}

/// A message to one client about its own request; other connections never
/// see it. Arrives in the same stream as the frames, told apart by `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// Pairing worked. The token is shown this once; Anya keeps only its hash.
    Paired {
        /// The client's id in Anya's list.
        client: String,
        /// Sent with every later connection.
        token: String,
        /// What the client may do until the user changes it.
        scopes: Vec<Scope>,
    },
    /// The session exists: send its audio as binary messages now.
    DictationAccepted {
        /// As sent in [`ClientMessage::DictationStart`].
        request_id: String,
        /// The session's id in every frame about it.
        session: SessionId,
        /// Whether partial results arrive while recording; false means the
        /// only result follows [`ClientMessage::DictationStop`].
        streaming: bool,
    },
    /// A message was not acted on.
    Refused {
        /// The request it answers, for `dictation_start`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        /// The session it names, for `dictation_stop` and `dictation_cancel`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
        /// Why, as a code apps translate.
        reason: Refusal,
        /// Untranslated specifics, for logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A reply type a newer Anya added.
    #[serde(other)]
    Unknown,
}

/// Why a client message was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The connection has no token, or a token Anya does not know.
    Unauthorized,
    /// The client's scopes do not allow it.
    Forbidden,
    /// This connection is already recording.
    Busy,
    /// The chosen engine lacks a required setting.
    NotConfigured,
    /// No session of this connection has that id.
    UnknownSession,
    /// Not a message Anya understands, or audio with no recording open.
    BadRequest,
    /// The pairing code is wrong, used or expired.
    PairingFailed,
    /// Too many wrong pairing codes; a new one must be shown.
    TooManyAttempts,
    /// A reason a newer Anya added.
    #[serde(other)]
    Unknown,
}

/// What a pairing QR code holds, as JSON: everything a client needs to
/// reach this Anya and trust it on first contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    /// The gateway to connect to: `wss://<lan address>:<port>/asr`.
    pub url: String,
    /// The one-time code for [`ClientMessage::Pair`].
    pub code: String,
    /// SHA-256 of the gateway's certificate (DER), lowercase hex pairs
    /// joined by `:`; the client accepts that certificate and no other.
    pub fingerprint: String,
}
