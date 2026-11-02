//! What Anya and the plugin host say to each other, and the registry index
//! Anya reads to offer plugins for install. Each plugin with a host entry
//! runs in a Bun process of its own; both directions are JSON lines on its
//! stdin and stdout (docs/decisions/0015-plugin-runtimes.md). Plugin authors
//! never see these messages: the `@wakaka6/anya-plugin` SDK speaks them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, Field, Manifest, Recognition, Source, Text};

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
        /// The engine's manifest, when the entry provides an engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<Manifest>,
        /// Everything the entry provides, the engine included.
        #[serde(default)]
        provides: Vec<Provide>,
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
    /// The answer to a `ping`: the host's event loop is free.
    Pong {
        /// The `ping`'s number.
        seq: u64,
    },
    /// A `context` call's answer.
    ContextDone {
        /// The call it answers.
        call: String,
        /// Words to add to the session's hotwords.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hotwords: Vec<String>,
        /// The language to open the session with.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// A `transform` call's answer.
    Transformed {
        /// The call it answers.
        call: String,
        /// The text that goes on.
        text: String,
    },
    /// An `action` call finished.
    ActionDone {
        /// The call it answers.
        call: String,
    },
    /// A hook call failed: the plugin threw, rejected or answered with the
    /// wrong shape.
    CallFailed {
        /// The call that failed.
        call: String,
        /// The plugin's own words, untranslated.
        detail: String,
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
    /// Answer with `pong` at once. Anya's watchdog sends one every second.
    Ping {
        /// Echoed in the `pong`.
        seq: u64,
    },
    /// Ask the plugin's context hook what a session should open with.
    /// Answered with `context_done` or `call_failed`.
    Context {
        /// Anya's id for the call, echoed in the answer.
        call: String,
        /// The package that provides the hook.
        plugin: String,
        /// The session the call is about.
        #[serde(flatten)]
        about: HookCall,
        /// The language the session would open with.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// The hotwords the session would open with.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hotwords: Vec<String>,
    },
    /// Run the plugin's transform hook on a session's final text.
    /// Answered with `transformed` or `call_failed`.
    Transform {
        /// Anya's id for the call, echoed in the answer.
        call: String,
        /// The package that provides the hook.
        plugin: String,
        /// The text so far.
        text: String,
        /// The session the text is from.
        #[serde(flatten)]
        about: HookCall,
    },
    /// Hand a session's final text to the plugin's action hook.
    /// Answered with `action_done` or `call_failed`.
    Action {
        /// Anya's id for the call, echoed in the answer.
        call: String,
        /// The package that provides the hook.
        plugin: String,
        /// The text, as the transforms left it.
        text: String,
        /// The session the text is from.
        #[serde(flatten)]
        about: HookCall,
    },
    /// Anya gave up on a call: its budget is spent. The plugin's signal is
    /// aborted, and an answer that still comes is ignored.
    Cancel {
        /// The call given up on.
        call: String,
    },
    /// Close every session and exit.
    Shutdown,
    /// A command a newer Anya added; a host that reads it ignores it.
    #[serde(other)]
    Unknown,
}

/// What every hook call says about its session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookCall {
    /// The session; absent for a `context` call, made before it opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// What opened the session.
    #[serde(default)]
    pub source: Source,
    /// The focused app's id (a macOS bundle id, an X11 window class), when
    /// the desktop tells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The plugin's settings, keyed by the field keys its package declares.
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
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
    /// What the plugin provides, as the registry's CI found in its entries.
    #[serde(default)]
    pub provides: Vec<Provide>,
    /// Whether the release has a host entry, and so runs in Bun. One without
    /// runs inline only: it cannot reach the network or the disk.
    #[serde(default)]
    pub host: bool,
    /// An engine plugin's `Manifest.streaming`, for listing before install.
    #[serde(default)]
    pub streaming: bool,
    /// What the plugin asks for besides the audio it receives: strings like
    /// `network:api.example.com`, shown to the user before they enable it.
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// One thing a plugin provides (docs/decisions/0016-dictation-hooks.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provide {
    /// A speech engine; it receives the audio of sessions opened on it.
    Engine,
    /// Hotwords and a language for a dictation before it opens; it learns the
    /// focused app.
    Context,
    /// Text in, text out, on a dictation's final text; it receives everything
    /// dictated.
    Transform,
    /// Something done with a dictation's final text beside the paste; it
    /// receives everything dictated.
    Action,
    /// A kind a newer Anya added; this one cannot install it.
    #[serde(other)]
    Unknown,
}

/// The `anya` field of a plugin's `package.json`: what Anya reads before it
/// runs any of the plugin's code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPackage {
    /// Plugin id; the package's `name` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Name to show; the package's `name` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Text>,
    /// One line under the name; the package's `description` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Text>,
    /// The module QuickJS runs inside Anya: context and transform hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
    /// The module Bun runs in a process of its own: an engine, hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Semver range of Anya versions it runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<String>,
    /// The [`PLUGIN_PROTOCOL`] it was built for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    /// Strings shown to the user before they enable it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    /// The hooks' settings, stored under the plugin's id; an engine keeps its
    /// own in its manifest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// The budgets the plugin asks for; the user's win.
    #[serde(default, skip_serializing_if = "Limits::is_empty")]
    pub limits: Limits,
}

/// Budgets of a plugin's hooks (docs/decisions/0015-plugin-runtimes.md):
/// what a package asks for, or what the user sets. Absent is the default;
/// every value is clamped to its hook's range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Milliseconds an inline call may run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_ms: Option<u32>,
    /// MiB an inline plugin's runtime may hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_memory_mib: Option<u32>,
    /// Milliseconds a host context call may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_ms: Option<u32>,
    /// Milliseconds a host transform call may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_ms: Option<u32>,
    /// Milliseconds an action may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_ms: Option<u32>,
}

impl Limits {
    /// Nothing set.
    pub fn is_empty(&self) -> bool {
        *self == Limits::default()
    }

    /// Each value of `self`, else `other`'s.
    pub fn or(self, other: Limits) -> Limits {
        Limits {
            inline_ms: self.inline_ms.or(other.inline_ms),
            inline_memory_mib: self.inline_memory_mib.or(other.inline_memory_mib),
            context_ms: self.context_ms.or(other.context_ms),
            transform_ms: self.transform_ms.or(other.transform_ms),
            action_ms: self.action_ms.or(other.action_ms),
        }
    }
}
