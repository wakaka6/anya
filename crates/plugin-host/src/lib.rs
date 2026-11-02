//! Community plugins: each enabled package with a host entry runs in a Bun
//! process of its own, and what it provides shows up in Anya as one more
//! [`anya_core::engine::Engine`] or context, transform or action hook
//! (docs/decisions/0015-plugin-runtimes.md, docs/decisions/0016-dictation-hooks.md).
//!
//! - [`Registry`] reads the anya-plugins index
//!   (docs/decisions/0010-anya-plugins-registry.md).
//! - [`Store`] installs a release into the plugins directory after checking
//!   its integrity hash, and records which plugins the user enabled.
//! - [`Bundled`] is a plugin built into Anya, which installs from the binary.
//! - [`read_package`] reads a package's `package.json`.
//! - [`BunPath`] finds or downloads the runtime.
//! - [`PluginHost`] runs the enabled plugins, watches them, and hands out
//!   their engines and hooks.
//!
//! Installing, enabling and updating are separate steps the user takes; this
//! crate never installs or updates by itself.

mod bun;
mod bundled;
mod engine;
mod error;
mod hooks;
mod host;
mod link;
mod package;
mod registry;
mod store;

pub use bun::{BunPath, BunRelease, MIN_BUN_VERSION, PINNED_BUN_VERSION};
pub use bundled::Bundled;
pub use engine::PluginEngine;
pub use error::PluginError;
pub use hooks::HostHook;
pub use host::{PluginHost, PluginState, PluginStatus};
pub use package::{Package, read_package};
pub use registry::{
    DEFAULT_INDEX_URL, Registry, find_release, integrity_of, parse_index, verify_integrity,
};
pub use store::{Installed, PluginDirs, Store};
