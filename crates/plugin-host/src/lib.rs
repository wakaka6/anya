//! Engine plugins: one Bun process runs the community's speech engines, and
//! each shows up in Anya as one more [`anya_core::engine::Engine`]
//! (docs/decisions/0004-plugins-in-bun.md).
//!
//! - [`Registry`] reads the anyahq-plugins index
//!   (docs/decisions/0010-anyahq-plugins-registry.md).
//! - [`Store`] installs a release into the plugins directory after checking
//!   its integrity hash, and records which plugins the user enabled.
//! - [`BunPath`] finds or downloads the runtime.
//! - [`PluginHost`] runs the enabled plugins and hands out their engines.
//!
//! Installing, enabling and updating are separate steps the user takes; this
//! crate never installs or updates by itself.

mod bun;
mod engine;
mod error;
mod host;
mod registry;
mod store;

pub use bun::{BunPath, BunRelease, MIN_BUN_VERSION, PINNED_BUN_VERSION};
pub use engine::PluginEngine;
pub use error::PluginError;
pub use host::{PluginHost, PluginState, PluginStatus};
pub use registry::{
    DEFAULT_INDEX_URL, Registry, find_release, integrity_of, parse_index, verify_integrity,
};
pub use store::{Installed, PluginDirs, Store};
