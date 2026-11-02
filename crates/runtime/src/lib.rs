//! What the settings window, the tray panel and the TUI may ask of a running
//! Anya: [`Control`], implemented by the binary, so the UIs never depend on
//! it and their tests run against a fake.
//!
//! Everything here is data or codes. The UIs translate [`Invalid`] and the
//! activity entries with `anya-i18n` ([`Invalid::message`] names the key).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anya_config::{Config, EngineRole};
use anya_core::audio::Device;
use anya_core::hooks::{HookFailure, Runs};
use anya_protocol::{
    EndReason, ErrorCode, Field, Manifest, PairingOffer, Provide, Scope, SessionId, Source, Text,
};
use serde::{Deserialize, Serialize};

/// Facts about this instance that do not change while it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Info {
    /// `macos` or `linux`.
    pub platform: &'static str,
    /// `darwin`, `wayland` or `x11`.
    pub desktop: String,
    /// Anya's version.
    pub version: String,
    /// The file saves go to.
    pub config_path: PathBuf,
    /// The hotwords file, next to the config file.
    pub hotwords_path: PathBuf,
    /// The built-in replacement list, next to the config file.
    pub replacements_path: PathBuf,
}

/// One engine as the settings show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineInfo {
    /// Its manifest.
    pub manifest: Manifest,
    /// The saved settings are enough to open a session.
    pub ready: bool,
    /// Whether a built-in engine is switched on; `None` for an engine with no
    /// switch (a community plugin's, which has its own on the Plugins page).
    pub enabled: Option<bool>,
}

/// What dictation is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Dictation {
    /// Waiting for a hotkey.
    Idle,
    /// Recording.
    Recording,
    /// The key is released; recording ends after the stop delay.
    Stopping,
    /// The last session failed; Esc dismisses, R retries.
    Failed {
        /// What went wrong.
        error: ErrorCode,
        /// Untranslated specifics.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Usage totals kept across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Stats {
    /// Dictation sessions that produced text.
    pub sessions: u64,
    /// Characters recognized.
    pub chars: u64,
    /// Audio recorded for them, in ms.
    pub audio_ms: u64,
    /// The latest session's characters.
    pub last_chars: u64,
    /// The latest session's audio, in ms.
    pub last_audio_ms: u64,
}

/// The subscriber server now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Subscription {
    /// Listening.
    Listening {
        /// What subscribers connect to.
        url: String,
        /// Connected now.
        subscribers: usize,
    },
    /// Not listening; why, untranslated (address taken, bad path).
    Down {
        /// The reason, for the log and beside a translated label.
        detail: String,
    },
}

/// The LAN listener now: the gateway paired apps reach over TLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Lan {
    /// LAN sharing is off; only this computer can connect.
    Off,
    /// Listening on the local network.
    Listening {
        /// What apps connect to: `wss://<lan address>:<port>/asr`.
        url: String,
        /// The certificate's SHA-256, as shown beside a pairing code.
        fingerprint: String,
    },
    /// LAN sharing is on but the listener is not up; why, untranslated.
    Down {
        /// The reason, for the log and beside a translated label.
        detail: String,
    },
}

/// A paired app, as the settings list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairedClient {
    /// Stable id, what the other methods take.
    pub id: String,
    /// The name the app gave when pairing.
    pub name: String,
    /// What it may do.
    pub scopes: Vec<Scope>,
    /// When it paired, unix ms.
    pub paired_at: u64,
    /// When it last connected or sent something, unix ms.
    pub last_seen: Option<u64>,
    /// Connections open now.
    pub connections: usize,
}

/// A pairing code just issued, with what the QR code shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pairing {
    /// The six digits to type into the app.
    pub code: String,
    /// When the code stops working, unix ms.
    pub expires_at: u64,
    /// The same code with the gateway's address and certificate, for a QR code.
    pub offer: PairingOffer,
}

/// Where an installed plugin is on its way to being usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PluginLoad {
    /// Installed and not enabled: it is not loaded and hears nothing.
    Off,
    /// Enabled; the plugin host is loading it.
    Loading,
    /// Loaded; its engine can open sessions.
    Ready,
    /// Enabled, but the plugin host process is not running.
    HostDown,
    /// Enabled but not loaded; why, in the plugin's own words.
    Failed {
        /// The reason, for the log and beside a translated label.
        detail: String,
    },
}

/// An installed plugin, as the settings list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledPlugin {
    /// The plugin id; also its engine's id when it provides one.
    pub id: String,
    /// The pinned version.
    pub version: String,
    /// The user turned it on; only then it loads and receives audio.
    pub enabled: bool,
    /// Whether it loaded.
    pub load: PluginLoad,
    /// Its name: the package's, or its engine's once it has loaded.
    pub title: Option<Text>,
    /// Its one-line description, from the package or its engine.
    pub description: Option<Text>,
    /// What it provides, as its package's entries export it; empty until
    /// it has loaded once.
    pub provides: Vec<Provide>,
    /// It has a host entry and runs in Bun with the user's rights; without
    /// one it runs inline only and cannot reach the network or the disk.
    pub host: bool,
    /// What it asks for, as its package declares it (`network:host`, …).
    pub permissions: Vec<String>,
    /// The settings of its hooks, kept under `[plugins.settings.<id>]`.
    pub fields: Vec<Field>,
    /// The keys of its required settings left blank; its hooks do not run
    /// until they are set.
    pub missing: Vec<String>,
}

/// A plugin the registry offers, at its newest release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MarketPlugin {
    /// The plugin id.
    pub id: String,
    /// Name to show.
    pub name: Text,
    /// One line under the name.
    pub description: Text,
    /// The newest release.
    pub version: String,
    /// The engine streams partial results.
    pub streaming: bool,
    /// What the release provides.
    pub provides: Vec<Provide>,
    /// The release runs in Bun; without a host entry it runs inline only.
    pub host: bool,
    /// What it asks for, as the registry declares it (`network:host`, …).
    pub permissions: Vec<String>,
}

/// One hook a dictation runs now, as the settings list them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookInfo {
    /// The plugin that provides it: a community plugin's id, or a built-in's.
    pub plugin: String,
    /// `context`, `transform` or `action`.
    pub hook: Provide,
    /// Where it runs.
    pub runs: Runs,
}

/// A built-in piece that serves hooks, with its switch
/// (`[plugins.builtin]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuiltinHooks {
    /// Its id: `replace` (the replacement list) or `community-hooks` (every
    /// community plugin's hooks).
    pub id: String,
    /// What it serves.
    pub provides: Vec<Provide>,
    /// Whether it runs.
    pub on: bool,
}

/// A running Anya's state, polled by the UIs a few times a second.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// Dictation's state.
    pub dictation: Dictation,
    /// Stopped sessions still waiting for their final result.
    pub finishing: usize,
    /// The text of the dictation session being recorded or finished.
    pub live_text: Option<String>,
    /// The input level (0.0 to 1.0) of the dictation session being recorded;
    /// `None` when none is.
    pub level: Option<f32>,
    /// Continuous transcription runs (the hotkey and tray switch included).
    pub continuous_on: bool,
    /// The subscriber server.
    pub subscription: Subscription,
    /// The LAN listener.
    pub lan: Lan,
    /// Usage totals.
    pub stats: Stats,
}

/// What the overlay preview shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewView {
    /// Recording, with sample text.
    Listening,
    /// A finished session.
    Done,
    /// A failed session.
    Error,
}

/// Why the overlay preview did not show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum PreviewIssue {
    /// The overlay is off or could not start.
    #[error("the overlay is not running")]
    NotRunning,
}

impl PreviewIssue {
    /// The catalog key of this issue's message.
    pub fn message(&self) -> &'static str {
        match self {
            PreviewIssue::NotRunning => "error.preview.not_running",
        }
    }
}

/// Where a session's final text went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivered {
    /// Pasted into the focused field.
    Pasted,
    /// Put on the clipboard.
    Copied,
    /// Kept: recognized and shown only (`output = none`, or continuous).
    Kept,
}

/// Something Anya says about itself in the activity list, as a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    /// Continuous transcription switched on.
    ContinuousOn,
    /// Continuous transcription switched off.
    ContinuousOff,
    /// Dictation hotkeys switched on.
    DictationOn,
    /// Dictation hotkeys switched off.
    DictationOff,
    /// The config file changed and was applied.
    ConfigReloaded,
    /// The config file changed but does not parse; the old one stays.
    ConfigBroken,
    /// The hotwords file could not be read.
    HotwordsUnreadable,
    /// Delivering the text failed; it may still be on the clipboard.
    DeliveryFailed,
    /// Plugins are enabled but there is no Bun to run them; the detail says
    /// what was missing.
    PluginsUnavailable,
    /// The recording overlay could not start; the detail says why.
    OverlayUnavailable,
}

/// What debug mode records. Never a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DebugEvent {
    /// A watched hotkey combo went down.
    KeyDown {
        /// The combo, in canonical names.
        combo: String,
    },
    /// A watched hotkey combo went up.
    KeyUp {
        /// The combo, in canonical names.
        combo: String,
    },
    /// How long the stages of a dictation session took, in ms; a stage that
    /// never happened is `None`.
    Timing {
        /// From the start of recording to the engine being connected.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connect_ms: Option<u64>,
        /// From the start of recording to the first words.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_text_ms: Option<u64>,
        /// From stopping to the final result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_ms: Option<u64>,
    },
}

/// One line of the activity list, kept on disk across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// A session's final text.
    Final {
        /// Unix ms.
        at: u64,
        /// What opened the session.
        source: Source,
        /// The text.
        text: String,
        /// Where it went.
        delivered: Delivered,
        /// The session, so the route page can point at this line. Lines
        /// written before it was kept have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
        /// The id of the engine that recognized it, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<String>,
    },
    /// A session failed.
    Failed {
        /// Unix ms.
        at: u64,
        /// What opened the session.
        source: Source,
        /// What went wrong.
        error: ErrorCode,
        /// Untranslated specifics.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Debug mode saw something; shown only while debug mode is on.
    Debug {
        /// Unix ms.
        at: u64,
        /// What.
        #[serde(flatten)]
        event: DebugEvent,
    },
    /// Anya said something about itself.
    Notice {
        /// Unix ms.
        at: u64,
        /// What.
        notice: Notice,
        /// Untranslated specifics (a path, an error).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A plugin's hook failed on a dictation. Never its text.
    HookFailed {
        /// Unix ms.
        at: u64,
        /// The plugin's id.
        plugin: String,
        /// Which hook.
        hook: Provide,
        /// Why, as a code.
        failure: HookFailure,
        /// The dictation it was running on, when there was one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
    },
    /// A plugin's Bun process stopped answering and was restarted.
    PluginStalled {
        /// Unix ms.
        at: u64,
        /// The plugin's id.
        plugin: String,
    },
}

/// The most messages [`Route::log`] keeps.
pub const ROUTE_LOG: usize = 200;

/// What the route page shows: the sessions open now, and the latest
/// messages between sources, engines and where the text goes. Kept in
/// memory only, never on disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Route {
    /// Sessions open now, oldest first.
    pub live: Vec<LiveRoute>,
    /// The latest messages, oldest first, at most [`ROUTE_LOG`].
    pub log: Vec<RouteEvent>,
}

/// One open session: where its audio comes from, the engine it goes to and
/// where its text goes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveRoute {
    /// The session.
    pub session: SessionId,
    /// Where its audio comes from.
    pub origin: Origin,
    /// The engine's manifest id.
    pub engine: String,
    /// Partial results arrive while audio is sent.
    pub streaming: bool,
    /// The latest audio chunk's level, 0.0 to 1.0.
    pub level: f32,
    /// The session's text so far.
    pub text: String,
    /// Where its text goes. Empty when nothing receives it.
    pub sinks: Vec<Sink>,
}

/// Where a session's audio comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    /// The user's own dictation.
    Dictation {
        /// The input device's name, when it is not the default.
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    /// Continuous transcription.
    Continuous {
        /// The device's name, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    /// A paired app dictating through the gateway.
    Remote {
        /// The app's name; empty while it is not known.
        client: String,
    },
}

/// Where a session's text goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sink {
    /// Pasted into the focused field.
    Paste,
    /// Put on the clipboard.
    Clipboard,
    /// Shown on the recording overlay.
    Overlay,
    /// Sent back to the paired app that dictated.
    Client {
        /// The app's name; empty while it is not known.
        name: String,
    },
    /// Sent to other apps subscribed to this source.
    Subscribers {
        /// How many were sent it.
        count: usize,
    },
}

/// One message on a session's route.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouteEvent {
    /// Increases by one per message.
    pub seq: u64,
    /// Unix ms.
    pub at: u64,
    /// The session it belongs to.
    pub session: SessionId,
    /// Where the session's audio comes from.
    pub origin: Origin,
    /// The engine's manifest id.
    pub engine: String,
    /// What happened.
    #[serde(flatten)]
    pub step: Step,
}

/// What a [`RouteEvent`] says happened, as its `step`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// The engine connected.
    Start {
        /// Partial results will arrive while audio is sent.
        streaming: bool,
    },
    /// Audio went to the engine.
    Audio {
        /// How much, in ms.
        ms: u64,
    },
    /// The engine sent a partial result.
    Partial {
        /// The session's text so far.
        text: String,
    },
    /// The engine sent its final result, and it went to `sinks`.
    Final {
        /// The session's whole text.
        text: String,
        /// Where it went; empty when nothing received it.
        sinks: Vec<Sink>,
    },
    /// The session failed.
    Failed {
        /// What went wrong.
        error: ErrorCode,
        /// Untranslated specifics.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// The session ended without a final result.
    Ended {
        /// Why.
        reason: EndReason,
    },
}

/// Which hotkey field a value is checked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyRole {
    /// A dictation hotkey.
    Dictation,
    /// The continuous transcription switch.
    Continuous,
}

/// A value a save refuses. Nothing is written when there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "invalid", rename_all = "snake_case")]
pub enum Invalid {
    /// A hotkey that does not parse.
    HotkeyUnparsable {
        /// As typed.
        value: String,
    },
    /// A key that types text (letters, digits, Space…) without Fn.
    HotkeyTextKey {
        /// The combo.
        combo: String,
    },
    /// Fn with F1–F24, which the system keeps for itself.
    HotkeyFnFunctionKey {
        /// The combo.
        combo: String,
    },
    /// The continuous switch reuses a dictation hotkey.
    HotkeyTaken {
        /// The combo.
        combo: String,
    },
    /// An overlay position that does not exist.
    UnknownPosition {
        /// As written.
        value: String,
    },
    /// An overlay scale that is not a positive number.
    BadScale,
    /// An engine Anya does not have, chosen for `role`.
    UnknownEngine {
        /// What it was chosen for.
        role: EngineRole,
        /// The id.
        id: String,
    },
    /// A subscription address Anya cannot listen on.
    BadAddress {
        /// As written.
        address: String,
        /// Why, untranslated.
        detail: String,
    },
    /// A plugin id in `[plugins]` that cannot name a plugin.
    BadPluginId {
        /// As written.
        id: String,
    },
}

impl Invalid {
    /// The `anya-i18n` key and its placeholder values.
    pub fn message(&self) -> (&'static str, Vec<(&'static str, String)>) {
        match self {
            Invalid::HotkeyUnparsable { value } => (
                "error.invalid.hotkey_unparsable",
                vec![("value", value.clone())],
            ),
            Invalid::HotkeyTextKey { combo } => (
                "error.invalid.hotkey_text_key",
                vec![("combo", combo.clone())],
            ),
            Invalid::HotkeyFnFunctionKey { combo } => (
                "error.invalid.hotkey_fn_function_key",
                vec![("combo", combo.clone())],
            ),
            Invalid::HotkeyTaken { combo } => {
                ("error.invalid.hotkey_taken", vec![("combo", combo.clone())])
            }
            Invalid::UnknownPosition { value } => {
                ("error.invalid.position", vec![("value", value.clone())])
            }
            Invalid::BadScale => ("error.invalid.scale", Vec::new()),
            Invalid::UnknownEngine { id, .. } => ("error.invalid.engine", vec![("id", id.clone())]),
            Invalid::BadAddress { address, detail } => (
                "error.invalid.address",
                vec![("address", address.clone()), ("detail", detail.clone())],
            ),
            Invalid::BadPluginId { id } => ("error.invalid.plugin_id", vec![("id", id.clone())]),
        }
    }
}

/// Why a save did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum SaveError {
    /// Values a save refuses; nothing was written.
    #[error("invalid settings: {0:?}")]
    Invalid(Vec<Invalid>),
    /// Writing the file failed; untranslated.
    #[error("cannot write the config: {0}")]
    Write(String),
}

/// A save that was written but not fully applied: the hotkeys could not be
/// registered (permission missing, another program holds them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Saved {
    /// Why applying fell short, untranslated; `None` when all applied.
    pub hotkeys_failed: Option<String>,
}

/// How the running Anya was installed, so the update notice says what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateVia {
    /// Under a Homebrew Cellar: Anya can run `brew upgrade anya`.
    Homebrew,
    /// Any other install: the user updates it where it came from.
    Other,
}

/// What the update check knows (docs/decisions/0014).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    /// Checking is off, or has not finished yet.
    Unknown,
    /// This is the newest version.
    Current,
    /// A newer version exists.
    Available {
        /// The newer version.
        version: String,
        /// How this Anya was installed.
        via: UpdateVia,
    },
    /// `brew upgrade anya` is running.
    Upgrading {
        /// The version being installed.
        version: String,
    },
    /// The upgrade finished; the new version runs after a restart.
    Restart {
        /// The installed version.
        version: String,
    },
    /// The check failed.
    Failed {
        /// The network's or the file system's words, untranslated.
        detail: String,
    },
}

/// Why an upgrade did not happen, as a code with its specifics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum UpdateIssue {
    /// This Anya was not installed with Homebrew.
    #[error("this Anya was not installed with Homebrew")]
    NotHomebrew,
    /// `brew` was not found.
    #[error("brew was not found")]
    BrewMissing,
    /// There is no newer version to install.
    #[error("there is no newer version")]
    NothingToInstall,
    /// `brew upgrade` failed.
    #[error("brew upgrade failed: {detail}")]
    Failed {
        /// The end of brew's own output.
        detail: String,
    },
}

/// Why a plugin operation did not happen, as a code with its specifics. The
/// UIs translate it with [`PluginIssue::message`]; the specifics (a URL, a
/// system error) stay as the system or the registry worded them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum PluginIssue {
    /// The registry index or an artifact could not be fetched.
    #[error("cannot fetch {url}: {reason}")]
    Fetch {
        /// What was asked for.
        url: String,
        /// The network's or the server's words.
        reason: String,
    },
    /// The registry index is not valid.
    #[error("the registry index is invalid: {reason}")]
    Index {
        /// What is wrong with it.
        reason: String,
    },
    /// The registry index has a format version this Anya does not read.
    #[error("the registry index has format version {found}, this Anya reads {supported}")]
    IndexVersion {
        /// The index's version.
        found: u32,
        /// The version Anya reads.
        supported: u32,
    },
    /// The registry has no such release.
    #[error("the registry has no plugin {id} version {version}")]
    NotInIndex {
        /// The plugin id.
        id: String,
        /// The version asked for.
        version: String,
    },
    /// The release cannot run here.
    #[error("plugin {id} {version} cannot run here: {reason}")]
    Incompatible {
        /// The plugin id.
        id: String,
        /// The release.
        version: String,
        /// What does not fit.
        reason: String,
    },
    /// A download does not match its hash.
    #[error("integrity check failed for {what}")]
    Integrity {
        /// The artifact checked.
        what: String,
    },
    /// An integrity string in an unsupported format.
    #[error("unsupported integrity string {value:?}")]
    IntegrityFormat {
        /// The string.
        value: String,
    },
    /// A plugin id or version that is not valid.
    #[error("invalid plugin id or version {value:?}")]
    Invalid {
        /// The offending text.
        value: String,
    },
    /// No installed plugin has this id.
    #[error("plugin {id} is not installed")]
    NotInstalled {
        /// The plugin id.
        id: String,
    },
    /// An archive that cannot be unpacked safely.
    #[error("cannot unpack {what}: {reason}")]
    Archive {
        /// The artifact.
        what: String,
        /// What is wrong with it.
        reason: String,
    },
    /// Reading or writing a file failed.
    #[error("{path}: {reason}")]
    Io {
        /// The file or directory.
        path: String,
        /// The system's words.
        reason: String,
    },
    /// No Bun can be downloaded for this platform.
    #[error("no Bun runtime for this platform: {reason}")]
    Bun {
        /// The asset or reason.
        reason: String,
    },
    /// Plugins need a Bun of at least this version and none was found.
    #[error("Bun {version} or newer was not found")]
    BunMissing {
        /// The oldest Bun that works.
        version: String,
    },
}

impl PluginIssue {
    /// The catalog key of this issue's message and its placeholders.
    pub fn message(&self) -> (&'static str, Vec<(&'static str, String)>) {
        match self {
            PluginIssue::Fetch { url, reason } => (
                "error.plugin.fetch",
                vec![("url", url.clone()), ("reason", reason.clone())],
            ),
            PluginIssue::Index { reason } => {
                ("error.plugin.index", vec![("reason", reason.clone())])
            }
            PluginIssue::IndexVersion { found, supported } => (
                "error.plugin.index_version",
                vec![
                    ("found", found.to_string()),
                    ("supported", supported.to_string()),
                ],
            ),
            PluginIssue::NotInIndex { id, version } => (
                "error.plugin.not_in_index",
                vec![("id", id.clone()), ("version", version.clone())],
            ),
            PluginIssue::Incompatible {
                id,
                version,
                reason,
            } => (
                "error.plugin.incompatible",
                vec![
                    ("id", id.clone()),
                    ("version", version.clone()),
                    ("reason", reason.clone()),
                ],
            ),
            PluginIssue::Integrity { what } => {
                ("error.plugin.integrity", vec![("what", what.clone())])
            }
            PluginIssue::IntegrityFormat { value } => (
                "error.plugin.integrity_format",
                vec![("value", value.clone())],
            ),
            PluginIssue::Invalid { value } => {
                ("error.plugin.invalid", vec![("value", value.clone())])
            }
            PluginIssue::NotInstalled { id } => {
                ("error.plugin.not_installed", vec![("id", id.clone())])
            }
            PluginIssue::Archive { what, reason } => (
                "error.plugin.archive",
                vec![("what", what.clone()), ("reason", reason.clone())],
            ),
            PluginIssue::Io { path, reason } => (
                "error.plugin.io",
                vec![("path", path.clone()), ("reason", reason.clone())],
            ),
            PluginIssue::Bun { reason } => ("error.plugin.bun", vec![("reason", reason.clone())]),
            PluginIssue::BunMissing { version } => (
                "error.plugin.bun_missing",
                vec![("version", version.clone())],
            ),
        }
    }
}

/// Why a change to a paired client did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum ClientIssue {
    /// No paired client has this id.
    #[error("no paired client {id:?}")]
    UnknownClient {
        /// The id asked for.
        id: String,
    },
    /// The clients file could not be written; nothing changed.
    #[error("saving the paired clients failed: {detail}")]
    Write {
        /// The system's words.
        detail: String,
    },
}

impl ClientIssue {
    /// The catalog key of this issue's message and its placeholders.
    pub fn message(&self) -> (&'static str, Vec<(&'static str, String)>) {
        match self {
            ClientIssue::UnknownClient { id } => ("error.client.unknown", vec![("id", id.clone())]),
            ClientIssue::Write { detail } => {
                ("error.client.write", vec![("detail", detail.clone())])
            }
        }
    }
}

/// Why no pairing code was issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "refused", rename_all = "snake_case")]
pub enum PairError {
    /// LAN sharing is off.
    #[error("LAN sharing is off")]
    LanOff,
    /// LAN sharing is on but its listener is not up.
    #[error("the LAN listener is down: {detail}")]
    LanDown {
        /// Why, untranslated (address taken, certificate unreadable).
        detail: String,
    },
}

impl PairError {
    /// The catalog key of this refusal's message and its placeholders.
    pub fn message(&self) -> (&'static str, Vec<(&'static str, String)>) {
        match self {
            PairError::LanOff => ("error.pairing.lan_off", Vec::new()),
            PairError::LanDown { detail } => {
                ("error.pairing.lan_down", vec![("detail", detail.clone())])
            }
        }
    }
}

/// A running Anya, as its UIs see it. Every method returns promptly: slow
/// work (stopping capture, reconnecting) happens in the background.
pub trait Control: Send + Sync {
    /// Facts about this instance.
    fn info(&self) -> Info;

    /// The config in effect.
    fn config(&self) -> Config;

    /// Checks `config`, writes it and applies it.
    fn save(&self, config: Config) -> Result<Saved, SaveError>;

    /// A hotkey list as typed, in canonical form (`Super+Alt` gives
    /// `Alt+Super`), or why it cannot be used for `role`.
    fn check_hotkeys(&self, value: &str, role: HotkeyRole) -> Result<String, Invalid>;

    /// Every engine, in the order to list them.
    fn engines(&self) -> Vec<EngineInfo>;

    /// Whether `settings` would let engine `id` open a session.
    fn engine_ready(&self, id: &str, settings: &BTreeMap<String, String>) -> bool;

    /// Input devices connected now; the error is untranslated.
    fn devices(&self) -> Result<Vec<Device>, String>;

    /// The state now.
    fn status(&self) -> Status;

    /// The activity list, oldest first.
    fn activity(&self) -> Vec<Entry>;

    /// Paired apps, oldest pairing first.
    fn clients(&self) -> Vec<PairedClient>;

    /// Issues a fresh pairing code (replacing any open one); refused while
    /// LAN sharing is off or its listener is down.
    fn start_pairing(&self) -> Result<Pairing, PairError>;

    /// Sets what client `id` may do; open connections follow at once. The
    /// error is untranslated.
    fn set_client_scopes(&self, id: &str, scopes: Vec<Scope>) -> Result<(), ClientIssue>;

    /// Unpairs client `id` and closes its connections. The error is untranslated.
    fn remove_client(&self, id: &str) -> Result<(), ClientIssue>;

    /// Installed plugins with their load state, in install order.
    fn plugins(&self) -> Vec<InstalledPlugin>;

    /// The registry's plugins at their newest release. Fetches the index,
    /// so it blocks on the network: call it off the UI thread. The error is
    /// translated by the UI.
    fn plugin_market(&self) -> Result<Vec<MarketPlugin>, PluginIssue>;

    /// Installs the newest release of plugin `id` from the registry,
    /// disabled. Blocks while downloading (the Bun runtime too, the first
    /// time).
    fn install_plugin(&self, id: &str) -> Result<(), PluginIssue>;

    /// Turns plugin `id` on or off and reloads the plugin host.
    fn set_plugin_enabled(&self, id: &str, on: bool) -> Result<(), PluginIssue>;

    /// Deletes plugin `id`.
    fn remove_plugin(&self, id: &str) -> Result<(), PluginIssue>;

    /// The hooks a dictation runs now, in the order it runs them: context
    /// and transform hooks by `[plugins] order`, then the actions.
    fn hooks(&self) -> Vec<HookInfo>;

    /// The built-in pieces that serve hooks, with their switches.
    fn builtin_hooks(&self) -> Vec<BuiltinHooks>;

    /// Turns dictation hotkeys on or off: saves `[dictation] enabled` and applies it.
    fn set_dictation(&self, on: bool) -> Result<Saved, SaveError>;

    /// Turns continuous transcription on or off: saves `[continuous] enabled`
    /// and switches the running service, also one its hotkey started.
    fn set_continuous(&self, on: bool) -> Result<Saved, SaveError>;

    /// Stops global hotkeys for `period` (a UI records a new hotkey and the
    /// old one must not start a session); calling again renews it, and they
    /// come back by themselves after it. `None` brings them back now.
    fn suspend_hotkeys(&self, period: Option<Duration>);

    /// Records the next key combination from the OS, which sees keys a
    /// webview cannot (Fn, Menu): global hotkeys pause meanwhile. Blocks the
    /// calling thread up to `timeout`; `Ok(None)` when Esc was pressed or time
    /// ran out. The combo is canonical; the error is untranslated.
    fn record_hotkey(&self, timeout: Duration) -> Result<Option<String>, String>;

    /// Shows `view` on the running overlay for a moment, then returns it to
    /// the real state. Nothing is recorded or recognized.
    fn preview_overlay(&self, view: PreviewView) -> Result<(), PreviewIssue>;

    /// Starts a new recording while a failure is shown (R on the overlay).
    fn retry_dictation(&self);

    /// The sessions open now and the latest messages on their routes.
    fn route(&self) -> Route {
        Route::default()
    }

    /// What the update check knows now.
    fn update(&self) -> UpdateState {
        UpdateState::Unknown
    }

    /// Checks for a newer version now, in the background, even when the
    /// daily check is off: the user asked. The answer shows in [`Control::update`].
    fn check_update(&self) {}

    /// Runs `brew upgrade anya` for a Homebrew install. Blocks until brew
    /// ends, so call it off the UI thread. The new version runs after a restart.
    fn upgrade(&self) -> Result<(), UpdateIssue> {
        Err(UpdateIssue::NotHomebrew)
    }

    /// Dismisses the failure being shown (Esc on the overlay).
    fn dismiss_dictation(&self);

    /// Puts `text` on the clipboard; the error is untranslated.
    fn copy(&self, text: &str) -> Result<(), String>;

    /// Ends Anya.
    fn quit(&self);
}
