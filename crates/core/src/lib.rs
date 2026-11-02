//! The core of Anya: the seams the rest of the workspace plugs into.
//!
//! Core knows no platform, engine, trigger or sink by name, and holds no text
//! for the user: errors are codes the app layer translates. [`audio`] is how a
//! device is captured, [`engine`] is how audio becomes
//! [`anya_protocol::Recognition`]s, and [`hub`] opens sessions and publishes
//! the [`anya_protocol::Frame`]s every reader sees. [`hooks`] are what plugins
//! add around a dictation: context before it opens, transforms and actions
//! after its final text.

pub mod audio;
pub mod desktop;
pub mod doctor;
pub mod engine;
pub mod hooks;
pub mod hub;
