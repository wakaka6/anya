//! What Anya and the plugin host say to each other, and the registry index
//! Anya reads to offer plugins for install. The host is one Bun process;
//! both directions are JSON lines on its stdin and stdout
//! (docs/decisions/0004-plugins-in-bun.md). Plugin authors never see these
//! messages: the `@wakaka6/anya-plugin` SDK speaks them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, Manifest, Recognition, Text};

/// Version of the host protocol and of the SDK API behind it. A registry
/// entry names the one it was built for; Anya installs and loads only
/// plugins of its own.
pub const PLUGIN_PROTOCOL: u32 = 1;

/// Version of the registry index format.
pub const INDEX_VERSION: u32 = 1;

/// Bytes of audio in one `audio` command before base64: 40 ms of signed
/// 16-bit little-endian mono PCM at 16 kHz.
pub const AUDIO_BYTES: usize = 1280;

/// A line the host writes to stdout. Unknown types read as
/// [`HostEvent::Unknown`] and are skipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostEvent {
    /// The first line: the host runs and takes commands.
    Ready {
        /// The host script's version.
        host_version: String,
        /// [`PLUGIN_PROTOCOL`] the host speaks.
        #[serde(default)]
        protocol: u32,
    },
    /// A plugin loaded and describes its engine.
    Loaded {
        /// The package id the `load` command named.
        plugin: String,
        /// The engine's manifest.
        manifest: Manifest,
    },
    /// A plugin did not load: missing entry, exception, or timeout.
    LoadFailed {
        /// The package id the `load` command named.
        plugin: String,
        /// The error's own words, untranslated.
        detail: String,
    },
    /// The plugin connected; results for `session` follow.
    SessionReady {
        /// The session the `open` command named.
        session: String,
    },
    /// A recognition result.
    Recognition {
        /// The session it belongs to.
        session: String,
        /// The result, its fields inline beside `type` and `session`.
        #[serde(flatten)]
        recognition: Recognition,
    },
    /// The session failed and is over.
    SessionError {
        /// The session that failed.
        session: String,
        /// What kind of failure.
        code: ErrorCode,
        /// The plugin's own words, untranslated; for `not_configured` the
        /// key of the missing setting.
        detail: String,
    },
    /// The session is over; nothing more names it.
    SessionClosed {
        /// The session that closed.
        session: String,
    },
    /// Something a plugin wrote to its console, or the host itself.
    Log {
        /// The package id, or `host`.
        plugin: String,
        /// How serious.
        level: LogLevel,
        /// The text.
        message: String,
    },
    /// A type a newer host added; skip it.
    #[serde(other)]
    Unknown,
}

/// How serious a [`HostEvent::Log`] line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Detail for debugging.
    Debug,
    /// Routine.
    Info,
    /// Worth a look.
    Warn,
    /// Something failed.
    Error,
    /// A level a newer host added.
    #[serde(other)]
    Unknown,
}

/// A line Anya writes to the host's stdin. Unknown types read as
/// [`HostCommand::Unknown`] and are skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostCommand {
    /// Load the plugin package unpacked at `path`. Answered with `loaded` or
    /// `load_failed`.
    Load {
        /// The package id, echoed in the answer.
        plugin: String,
        /// The package directory; its `package.json` has an `anya` field.
        path: String,
    },
    /// Open a recognition session. Answered with `session_ready`, or with
    /// `session_error`.
    Open {
        /// Anya's id for the session, echoed in every event about it.
        session: String,
        /// The package that provides the engine.
        plugin: String,
        /// The engine's manifest id.
        engine: String,
        /// BCP-47 tag; absent leaves the language to the engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// Words to favour.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hotwords: Vec<String>,
        /// The engine's settings, keyed by its manifest's field keys.
        #[serde(default)]
        settings: BTreeMap<String, String>,
    },
    /// More audio for an open session: PCM as in [`AUDIO_BYTES`], base64.
    Audio {
        /// The session it belongs to.
        session: String,
        /// Standard base64 of the PCM bytes.
        pcm: String,
    },
    /// No more audio: the plugin sends its final result, then the session
    /// ends.
    Finish {
        /// The session that finished.
        session: String,
    },
    /// Abandon the session; no result is wanted.
    Close {
        /// The session to close.
        session: String,
    },
    /// Close every session and exit.
    Shutdown,
    /// A command a newer Anya added; a host that reads it ignores it.
    #[serde(other)]
    Unknown,
}

/// The registry index: every plugin the registry offers and its released
/// versions. CI of the registry repository builds it; Anya only reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// [`INDEX_VERSION`] of the format. Anya refuses another one.
    pub version: u32,
    /// The plugins, in the order to show them.
    #[serde(default)]
    pub plugins: Vec<IndexPlugin>,
}

/// One plugin in the [`Index`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexPlugin {
    /// Stable id: lowercase ASCII letters, digits and `-`; also the
    /// directory the plugin installs into.
    pub id: String,
    /// Name to show.
    pub name: Text,
    /// One line under the name.
    #[serde(default, skip_serializing_if = "Text::is_empty")]
    pub description: Text,
    /// Released versions, oldest first.
    #[serde(default)]
    pub versions: Vec<IndexVersion>,
}

/// One release of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexVersion {
    /// Semver of the release.
    pub version: String,
    /// URL of the release's npm tarball (`.tgz`).
    pub artifact: String,
    /// Subresource Integrity hash of the tarball: `sha512-<base64>`.
    pub integrity: String,
    /// Semver range of Anya versions the release runs on.
    pub anya: String,
    /// The [`PLUGIN_PROTOCOL`] it was built for.
    pub protocol: u32,
    /// What the plugin provides.
    pub kind: PluginKind,
    /// An engine plugin's `Manifest.streaming`, for listing before install.
    #[serde(default)]
    pub streaming: bool,
    /// What the plugin asks for besides the audio it receives: strings like
    /// `network:api.example.com`, shown to the user before they enable it.
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// What a plugin provides, as `kind` in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// A speech engine; it receives the audio of sessions opened on it.
    Engine,
    /// A kind a newer Anya added; this one cannot install it.
    #[serde(other)]
    Unknown,
}
