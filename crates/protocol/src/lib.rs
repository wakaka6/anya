//! The wire formats of Anya: everything that crosses a process boundary.
//!
//! Subscribers read [`Frame`]s as JSON text messages. Engine plugins send a
//! [`Manifest`] when they load and [`Recognition`]s while they recognize, the
//! same shape a `result` frame carries, so plugin authors and app authors
//! learn one result format.
//!
//! Gateway clients also send [`ClientMessage`]s and binary audio, and get
//! [`Reply`]s about their own requests in the same stream as the frames.
//!
//! Compatibility: while [`PROTOCOL_VERSION`] stays the same, frame types and
//! fields are only added. Consumers ignore types and fields they do not know.
//! Removing or renaming anything bumps the version.
//!
//! docs/protocol.md is the subscriber's guide. [`contract::check`] holds the
//! ordering promises it makes, and `fixtures/` has streams that keep them.

mod client;
pub mod contract;
mod frame;
mod manifest;
mod plugin;
mod text;

pub use client::{
    ClientMessage, PairingOffer, Refusal, Reply, SUBPROTOCOL, Scope, TOKEN_SUBPROTOCOL_PREFIX,
    UPLOAD_SAMPLE_RATE,
};
pub use frame::{
    EndReason, ErrorCode, Frame, Recognition, SessionId, Source, SourceState, Utterance,
};
pub use manifest::{Field, FieldKind, Manifest};
pub use plugin::{
    AUDIO_BYTES, HookCall, HostCommand, HostEvent, INDEX_VERSION, Index, IndexPlugin, IndexVersion,
    Limits, LogLevel, PLUGIN_PROTOCOL, PluginPackage, Provide,
};
pub use text::Text;

/// Version of the formats in this crate, announced in [`Frame::Hello`].
/// It stays 0 until the first release; until then any change may break consumers.
pub const PROTOCOL_VERSION: u32 = 0;
