//! The `anya` binary's parts: the command line, the environment check,
//! `--install`, and what it keeps on disk. `main.rs` wires them to every
//! other crate.

pub mod cli;
pub mod doctor;
pub mod install;
pub mod store;
