//! Capture on Linux and macOS behind anya-core's AudioSource.
//!
//! [`System`] records microphones through cpal's native host and records
//! system sound through the default output's monitor: PulseAudio/PipeWire on
//! Linux, CoreAudio process taps on macOS. [`Replay`] plays a WAV file as if it were a
//! device, for demos and smoke tests without a microphone. Both deliver
//! [`anya_core::audio::Chunk`]s of exactly
//! [`anya_core::audio::CHUNK_BYTES`], whatever the source's format.
//! [`checks`] tells the doctor whether this machine can record.

mod classify;
mod convert;
mod doctor;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
mod replay;
mod select;
mod system;

pub use doctor::checks;
pub use replay::Replay;
pub use system::System;
