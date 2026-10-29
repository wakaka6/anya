//! What the settings window, the tray panel and the TUI may ask of a running
//! Anya: [`Control`], implemented by the binary, so the UIs never depend on
//! it and their tests run against a fake.
//!
//! Everything here is data or codes. The UIs translate [`Invalid`] and the
//! activity entries with `anya-i18n` ([`Invalid::message`] names the key).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anya_config::Config;
use anya_core::audio::Device;
use anya_protocol::{ErrorCode, Manifest, Source};
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
    /// Started with `--debug`.
    pub debug: bool,
}

/// One engine as the settings show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineInfo {
    /// Its manifest.
    pub manifest: Manifest,
    /// The saved settings are enough to open a session.
    pub ready: bool,
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

/// A running Anya's state, polled by the UIs a few times a second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// Dictation's state.
    pub dictation: Dictation,
    /// Stopped sessions still waiting for their final result.
    pub finishing: usize,
    /// The text of the dictation session being recorded or finished.
    pub live_text: Option<String>,
    /// Continuous transcription runs (the hotkey and tray switch included).
    pub continuous_on: bool,
    /// The subscriber server.
    pub subscription: Subscription,
    /// Usage totals.
    pub stats: Stats,
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
    /// An engine Anya does not have.
    UnknownEngine {
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
            Invalid::UnknownEngine { id } => ("error.invalid.engine", vec![("id", id.clone())]),
            Invalid::BadAddress { address, detail } => (
                "error.invalid.address",
                vec![("address", address.clone()), ("detail", detail.clone())],
            ),
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

    /// Puts `text` on the clipboard; the error is untranslated.
    fn copy(&self, text: &str) -> Result<(), String>;

    /// Ends Anya.
    fn quit(&self);
}
