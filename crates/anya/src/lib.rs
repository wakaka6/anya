//! The `anya` binary's parts: the command line, the environment check,
//! `--install`, the plugin commands, and what it keeps on disk. `main.rs` wires them to every
//! other crate.

pub mod builtin;
pub mod cli;
pub mod community;
pub mod doctor;
pub mod icons;
pub mod indicator;
pub mod install;
pub mod official;
pub mod plugins;
pub mod store;
