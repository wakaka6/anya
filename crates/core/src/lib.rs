//! The core of Anya: the seams the rest of the workspace plugs into.
//!
//! Core knows no platform, engine, trigger or sink by name. [`audio`] is how a
//! device is captured and [`engine`] is how audio becomes
//! [`anya_protocol::Recognition`]s. The session controller that joins the two
//! and publishes [`anya_protocol::Frame`]s belongs here too (ARCHITECTURE.md,
//! "Session flow").

pub mod audio;
pub mod engine;
