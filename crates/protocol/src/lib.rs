//! The wire formats of Anya: everything that crosses a process boundary.
//!
//! Subscribers read [`Frame`]s as JSON text messages. Engine plugins send a
//! [`Manifest`] when they load and [`Recognition`]s while they recognize, the
//! same shape a `result` frame carries, so plugin authors and app authors
//! learn one result format.
//!
//! Compatibility: while [`PROTOCOL_VERSION`] stays the same, frame types and
//! fields are only added. Consumers ignore types and fields they do not know.
//! Removing or renaming anything bumps the version.
//!
//! docs/protocol.md is the subscriber's guide. [`contract::check`] holds the
//! ordering promises it makes, and `fixtures/` has streams that keep them.

pub mod contract;
mod frame;
mod manifest;

pub use frame::{EndReason, Frame, Recognition, SessionId, Source, SourceState, Utterance};
pub use manifest::{Field, FieldKind, Manifest};

/// Version of the formats in this crate, announced in [`Frame::Hello`].
/// It stays 0 until the first release; until then any change may break consumers.
pub const PROTOCOL_VERSION: u32 = 0;
