//! The config file: `~/.config/anya/config.toml` (or `$XDG_CONFIG_HOME/anya`)
//! on Linux and macOS alike.
//!
//! Keys missing from the file take their defaults; unknown keys are kept out
//! of the way, not errors. Saving is atomic and leaves the file readable by
//! its owner only, since it holds engine credentials. [`watch`] reports every
//! change of the file's contents, including an atomic save by an editor.
//!
//! This crate holds no user text: [`Problem`]s are codes the UIs translate.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anya_core::hooks::{Budgets, chain_budget};
use anya_protocol::Limits;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

mod hotwords;
mod replacements;

pub use hotwords::{HotwordsFile, read_hotwords};
pub use replacements::{
    REPLACEMENTS_FILE, Replacement, ReplacementsFile, parse_replacements, read_replacements,
    replacements_path,
};

/// The engine used when the config names none.
pub const DEFAULT_ENGINE: &str = "doubao";
/// The subscription address used when the config names none.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:7590";
/// The LAN sharing port used when the config names none.
pub const DEFAULT_LAN_PORT: u16 = 7591;
/// Recognition languages the UIs offer, BCP-47; empty is auto-detect. A
/// configured tag outside the list is kept and shown as itself.
pub const LANGUAGES: &[&str] = &[
    "", "zh-CN", "en-US", "ja-JP", "ko-KR", "fr-FR", "de-DE", "es-ES", "ru-RU",
];

/// The whole config file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The interface.
    pub ui: Ui,
    /// Hotkey dictation into the focused field.
    pub dictation: Dictation,
    /// Transcribing what the computer plays, or another device, for subscribers.
    pub continuous: Continuous,
    /// Which engine recognizes, and what it is told.
    pub recognition: Recognition,
    /// Capture shared by dictation and continuous transcription.
    pub audio: Audio,
    /// The subscriber server.
    pub subscription: Subscription,
    /// Engine plugins.
    pub plugins: Plugins,
    /// Looking for a newer Anya.
    pub updates: Updates,
    /// The recording overlay.
    pub overlay: Overlay,
    /// Development aids.
    pub debug: Debug,
}

/// `[ui]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// A BCP-47 tag, or `system` for the system's language.
    pub language: String,
}

impl Default for Ui {
    fn default() -> Ui {
        Ui {
            language: "system".to_owned(),
        }
    }
}

/// How a dictation hotkey starts and ends a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Press to start, press again to stop.
    #[default]
    Toggle,
    /// Record while held.
    Hold,
}

/// Where dictated text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// Into the focused field (through the clipboard).
    #[default]
    Paste,
    /// Onto the clipboard only.
    Clipboard,
    /// Nowhere: recognized, logged and shown, not delivered.
    None,
}

/// `[dictation]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dictation {
    /// Dictation hotkeys answer.
    pub enabled: bool,
    /// Comma-separated hotkeys, e.g. `"Fn, F13"`; empty means no dictation.
    pub hotkeys: String,
    /// Toggle or hold.
    pub mode: Mode,
    /// In hold mode, how long recording goes on after the release.
    pub stop_delay_ms: u32,
    /// Input device id or name; empty is the system default.
    pub device: String,
    /// Where the text goes.
    pub output: Output,
}

impl Default for Dictation {
    fn default() -> Dictation {
        Dictation {
            enabled: true,
            hotkeys: if cfg!(target_os = "macos") {
                "Option+Command"
            } else {
                "Alt+Super"
            }
            .to_owned(),
            mode: Mode::Toggle,
            stop_delay_ms: 0,
            device: String::new(),
            output: Output::Paste,
        }
    }
}

/// `[continuous]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Continuous {
    /// The service runs. Its hotkey and the tray switch change the running
    /// state without writing this.
    pub enabled: bool,
    /// A hotkey that switches the service on and off; empty for none.
    pub hotkey: String,
    /// Input device id or name; empty is system sound, what the computer plays
    /// through its default output.
    pub device: String,
    /// Cut speech into sessions at silences, uploading nothing while quiet.
    pub silence_gate: bool,
}

impl Default for Continuous {
    fn default() -> Continuous {
        Continuous {
            enabled: false,
            hotkey: String::new(),
            device: String::new(),
            silence_gate: true,
        }
    }
}

/// `[recognition]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recognition {
    /// The engine id dictation sessions open on.
    pub dictation: String,
    /// The engine id continuous transcription opens on.
    pub continuous: String,
    /// The engine id paired apps' remote dictation opens on.
    pub remote: String,
    /// BCP-47 tag; empty leaves it to the engine.
    pub language: String,
    /// Words to favour.
    pub hotwords: Vec<String>,
    /// A file of more hotwords, one per line, `#` comments, `~` expanded;
    /// read at every session, so another program can keep it current.
    pub hotwords_file: String,
    /// Each engine's settings under its id, kept when another is chosen.
    pub engines: BTreeMap<String, BTreeMap<String, String>>,
}

impl Default for Recognition {
    fn default() -> Recognition {
        Recognition {
            dictation: DEFAULT_ENGINE.to_owned(),
            continuous: DEFAULT_ENGINE.to_owned(),
            remote: DEFAULT_ENGINE.to_owned(),
            language: "zh-CN".to_owned(),
            hotwords: Vec::new(),
            hotwords_file: String::new(),
            engines: BTreeMap::new(),
        }
    }
}

impl Recognition {
    /// The engine id chosen for `role`; the default when blank.
    pub fn engine_id(&self, role: EngineRole) -> &str {
        let id = match role {
            EngineRole::Dictation => &self.dictation,
            EngineRole::Continuous => &self.continuous,
            EngineRole::Remote => &self.remote,
        };
        match id.trim() {
            "" => DEFAULT_ENGINE,
            id => id,
        }
    }

    /// Sets the engine for `role`.
    pub fn set_engine(&mut self, role: EngineRole, id: &str) {
        let slot = match role {
            EngineRole::Dictation => &mut self.dictation,
            EngineRole::Continuous => &mut self.continuous,
            EngineRole::Remote => &mut self.remote,
        };
        id.clone_into(slot);
    }

    /// Every engine id in use, each once, in role order.
    pub fn engines_in_use(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        for role in EngineRole::ALL {
            let id = self.engine_id(role);
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    /// The settings of engine `id`; empty when it has none.
    pub fn settings(&self, id: &str) -> BTreeMap<String, String> {
        self.engines.get(id).cloned().unwrap_or_default()
    }

    /// The language as a session wants it.
    pub fn language(&self) -> Option<String> {
        Some(self.language.trim().to_owned()).filter(|l| !l.is_empty())
    }
}

/// What a session is for; each picks its own engine under `[recognition]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineRole {
    /// Hotkey dictation on this computer.
    Dictation,
    /// Continuous transcription for subscribers.
    Continuous,
    /// Dictation sent by paired apps over the gateway.
    Remote,
}

impl EngineRole {
    /// Every role, in the order UIs list them.
    pub const ALL: [EngineRole; 3] = [
        EngineRole::Dictation,
        EngineRole::Continuous,
        EngineRole::Remote,
    ];

    /// The key under `[recognition]`, also the field name UIs report.
    pub fn key(self) -> &'static str {
        match self {
            EngineRole::Dictation => "dictation",
            EngineRole::Continuous => "continuous",
            EngineRole::Remote => "remote",
        }
    }
}

/// `[audio]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    /// Input gain in dB added to every capture.
    pub gain: i32,
}

/// `[subscription]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Subscription {
    /// Loopback `host:port` or `unix:<path>`; empty is the default.
    pub address: String,
    /// LAN sharing: also accept paired apps on the local network over TLS.
    /// Off by default; the loopback address above is unaffected.
    pub lan: bool,
    /// The TCP port of the LAN listener.
    pub lan_port: u16,
}

impl Default for Subscription {
    fn default() -> Subscription {
        Subscription {
            address: DEFAULT_ADDRESS.to_owned(),
            lan: false,
            lan_port: DEFAULT_LAN_PORT,
        }
    }
}

impl Subscription {
    /// The address to listen on, the default when blank.
    pub fn address(&self) -> &str {
        match self.address.trim() {
            "" => DEFAULT_ADDRESS,
            a => a,
        }
    }
}

/// `[plugins]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Plugins {
    /// URL (`https://` or `file://`) of the registry index; empty is the
    /// anya-plugins registry.
    pub index: String,
    /// The user's choice per built-in plugin id: `false` turns it off. A
    /// plugin with no row is on, and one in use stays on whatever its row says
    /// (docs/decisions/0012).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub builtin: BTreeMap<String, bool>,
    /// Plugin ids in the order their context and transform hooks run; those
    /// not listed run after, by id (docs/decisions/0016).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    /// `[plugins.limits]`
    #[serde(skip_serializing_if = "PluginLimits::is_empty")]
    pub limits: PluginLimits,
    /// `[plugins.settings.<id>]`: a plugin's hook settings, by the keys of the
    /// fields its package declares.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, BTreeMap<String, String>>,
}

impl Plugins {
    /// The index URL when the config names one.
    pub fn index(&self) -> Option<&str> {
        Some(self.index.trim()).filter(|u| !u.is_empty())
    }

    /// The settings of plugin `id`; empty when it has none.
    pub fn settings_of(&self, id: &str) -> BTreeMap<String, String> {
        self.settings.get(id).cloned().unwrap_or_default()
    }

    /// Where `id` stands in the user's order: its index in `order`, else after
    /// every listed plugin. Sort by this, then by id, for the run order.
    pub fn rank(&self, id: &str) -> usize {
        self.order
            .iter()
            .position(|o| o == id)
            .unwrap_or(self.order.len())
    }

    /// The budgets of plugin `id`: the user's for it, else for every plugin,
    /// else `asked` by its package, else the defaults; clamped.
    pub fn budgets(&self, id: &str, asked: Limits) -> Budgets {
        Budgets::resolve(self.limits.of(id), self.limits.every, asked)
    }

    /// The whole transform chain's budget.
    pub fn chain_budget(&self) -> Duration {
        chain_budget(self.limits.chain_ms)
    }
}

/// `[plugins.limits]`: the budgets of every plugin (keys of [`Limits`]), the
/// transform chain's (`chain_ms`), and one table per plugin id with a
/// plugin's own. A misspelt key is an error. Values are clamped where they
/// are used, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PluginLimits {
    /// What every plugin gets unless it has its own.
    #[serde(flatten)]
    pub every: Limits,
    /// Milliseconds the whole transform chain may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_ms: Option<u32>,
    /// One plugin's own, by plugin id.
    #[serde(flatten)]
    pub plugins: BTreeMap<String, Limits>,
}

impl PluginLimits {
    /// Nothing set.
    pub fn is_empty(&self) -> bool {
        *self == PluginLimits::default()
    }

    /// Plugin `id`'s own limits; nothing set when it has none.
    pub fn of(&self, id: &str) -> Limits {
        self.plugins.get(id).copied().unwrap_or_default()
    }
}

/// `[updates]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Updates {
    /// Ask the tap once a day whether a newer Anya exists
    /// (docs/decisions/0014). Off, Anya makes no such request.
    pub check: bool,
    /// URL (`https://` or `file://`) of the formula whose `version` is the
    /// newest; empty is the default tap's.
    pub formula: String,
}

impl Default for Updates {
    fn default() -> Updates {
        Updates {
            check: true,
            formula: String::new(),
        }
    }
}

impl Updates {
    /// The formula URL when the config names one.
    pub fn formula(&self) -> Option<&str> {
        Some(self.formula.trim()).filter(|u| !u.is_empty())
    }
}

/// `[overlay]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overlay {
    /// The overlay is shown.
    pub enabled: bool,
    /// `notch`, `top-left`, `top-center`, `top-right`, `bottom-left`,
    /// `bottom-center` or `bottom-right`.
    pub position: String,
    /// Shown while idle too.
    pub idle_visible: bool,
    /// Shows recognized text.
    pub show_text: bool,
    /// Size factor; 1.0 normal.
    pub scale: f64,
}

/// Overlay positions, in the order UIs list them.
pub const POSITIONS: &[&str] = &[
    "notch",
    "top-left",
    "top-center",
    "top-right",
    "bottom-left",
    "bottom-center",
    "bottom-right",
];

impl Default for Overlay {
    fn default() -> Overlay {
        Overlay {
            enabled: true,
            position: if cfg!(target_os = "macos") {
                "notch"
            } else {
                "bottom-center"
            }
            .to_owned(),
            idle_visible: false,
            show_text: true,
            scale: 1.0,
        }
    }
}

/// `[debug]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Debug {
    /// The activity log also records hotkey edges and the timing of each
    /// dictation session.
    pub enabled: bool,
    /// Extra hotkeys whose edges are recorded while `enabled`.
    pub hotkeys: Vec<String>,
}

/// Something in a config that cannot be used as written. UIs refuse to save
/// it and translate the code; a hand-edited file with one still loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// `[overlay] position` names no position.
    UnknownPosition(String),
    /// `[overlay] scale` is not a positive number.
    BadScale,
    /// The engine for a role names no engine Anya has.
    UnknownEngine(EngineRole, String),
    /// A plugin id in `[plugins]` that cannot name a plugin: blank, or with
    /// spaces around it.
    BadPluginId(PluginSection, String),
}

/// Where in `[plugins]` a plugin id was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSection {
    /// `order`
    Order,
    /// `limits.<id>`
    Limits,
    /// `settings.<id>`
    Settings,
}

impl Config {
    /// What cannot be used as written. `engines` are the ids Anya has. Hotkeys
    /// and the subscription address are checked by the crates that read them.
    pub fn problems(&self, engines: &[&str]) -> Vec<Problem> {
        let mut out = Vec::new();
        if !POSITIONS.contains(&self.overlay.position.as_str()) {
            out.push(Problem::UnknownPosition(self.overlay.position.clone()));
        }
        if !(self.overlay.scale.is_finite() && self.overlay.scale > 0.0) {
            out.push(Problem::BadScale);
        }
        for role in EngineRole::ALL {
            let engine = self.recognition.engine_id(role);
            if !engines.contains(&engine) {
                out.push(Problem::UnknownEngine(role, engine.to_owned()));
            }
        }
        let bad = |id: &&&str| id.is_empty() || id.trim() != **id;
        let plugins = &self.plugins;
        let ids = [
            (
                PluginSection::Order,
                plugins.order.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
            (
                PluginSection::Limits,
                plugins.limits.plugins.keys().map(String::as_str).collect(),
            ),
            (
                PluginSection::Settings,
                plugins.settings.keys().map(String::as_str).collect(),
            ),
        ];
        for (section, ids) in ids {
            for id in ids.iter().filter(bad) {
                out.push(Problem::BadPluginId(section, (*id).to_owned()));
            }
        }
        out
    }
}

/// Why the config could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading or writing the file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
    /// The file is not valid TOML for this shape; carries the parser's message.
    #[error("{path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// Line, column and what was expected.
        message: String,
    },
    /// No home directory to put the config in.
    #[error("no home directory")]
    NoHome,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir).join("anya")),
        None => home().map(|h| h.join(fallback).join("anya")),
    }
}

/// `$XDG_CONFIG_HOME/anya/config.toml`, else `~/.config/anya/config.toml`.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    xdg("XDG_CONFIG_HOME", ".config")
        .map(|d| d.join("config.toml"))
        .ok_or(ConfigError::NoHome)
}

/// Where stats, the activity log and the log file live:
/// `$XDG_STATE_HOME/anya`, else `~/.local/state/anya`.
pub fn state_dir() -> Result<PathBuf, ConfigError> {
    xdg("XDG_STATE_HOME", ".local/state").ok_or(ConfigError::NoHome)
}

/// Where plugins and the runtime that runs them live:
/// `$XDG_DATA_HOME/anya`, else `~/.local/share/anya`.
pub fn data_dir() -> Result<PathBuf, ConfigError> {
    xdg("XDG_DATA_HOME", ".local/share").ok_or(ConfigError::NoHome)
}

/// Where a running instance says where it listens: `$XDG_RUNTIME_DIR/anya`,
/// else the state directory.
pub fn runtime_dir() -> Result<PathBuf, ConfigError> {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir).join("anya")),
        None => state_dir(),
    }
}

/// A leading `~` or `~/` as the home directory; anything else unchanged.
pub fn expand_home(path: &str) -> PathBuf {
    let path = path.trim();
    match (path.strip_prefix('~'), home()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

/// Reads `path`. A missing file is the defaults.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn parse(path: &Path, text: &str) -> Result<Config, ConfigError> {
    toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

/// Writes `config` to `path` atomically, readable by its owner only, creating
/// the directory. A crash mid-save leaves the old file.
pub fn save(config: &Config, path: &Path) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    let text = toml::to_string_pretty(config).map_err(|e| ConfigError::Parse {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(io)?;
    let name = path
        .file_name()
        .map_or_else(|| "config.toml".into(), |n| n.to_string_lossy());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = open_private(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(io)
}

fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// How often [`watch`] looks at the file.
pub const WATCH_INTERVAL: Duration = Duration::from_millis(500);

/// Watches `path` and sends the config each time the file's contents change,
/// by any writer: in place, by an atomic rename, or created after start. A
/// file that stops parsing sends the error once and keeps watching. A file
/// that disappears is ignored until it returns. Ends when the receiver drops.
/// Must be called inside a Tokio runtime.
pub fn watch(path: PathBuf) -> mpsc::Receiver<Result<Config, ConfigError>> {
    let (tx, rx) = mpsc::channel(4);
    // Read now, not in the task: a save right after this call is a change.
    let mut seen = std::fs::read(&path).ok();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(WATCH_INTERVAL);
        loop {
            tick.tick().await;
            if tx.is_closed() {
                return;
            }
            let Ok(now) = std::fs::read(&path) else {
                continue;
            };
            if seen.as_deref() == Some(now.as_slice()) {
                continue;
            }
            let read = parse(&path, &String::from_utf8_lossy(&now));
            seen = Some(now);
            if tx.send(read).await.is_err() {
                return;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "anya-config-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_and_missing_keys_take_the_defaults() {
        let dir = temp();
        assert_eq!(
            load(&dir.join("none.toml")).expect("load"),
            Config::default()
        );
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("partial.toml");
        std::fs::write(&path, "[dictation]\nmode = \"hold\"\n").expect("write");
        let cfg = load(&path).expect("load");
        assert_eq!(cfg.dictation.mode, Mode::Hold);
        assert_eq!(cfg.dictation.output, Output::Paste);
        assert!(cfg.continuous.silence_gate);
        assert_eq!(cfg.subscription.address(), DEFAULT_ADDRESS);
    }

    #[test]
    fn lan_sharing_is_off_until_asked_for() {
        let dir = temp();
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("lan.toml");
        let cfg = Config::default();
        assert!(!cfg.subscription.lan);
        assert_eq!(cfg.subscription.lan_port, 7591);
        std::fs::write(&path, "[subscription]\nlan = true\nlan_port = 9000\n").expect("write");
        let cfg = load(&path).expect("load");
        assert!(cfg.subscription.lan);
        assert_eq!(cfg.subscription.lan_port, 9000);
        assert_eq!(cfg.subscription.address(), DEFAULT_ADDRESS);
    }

    #[test]
    fn the_plugin_index_is_the_registry_unless_named() {
        let dir = temp();
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("plugins.toml");
        assert_eq!(Config::default().plugins.index(), None);
        std::fs::write(&path, "[plugins]\nindex = \" file:///i.json \"\n").expect("write");
        let cfg = load(&path).expect("load");
        assert_eq!(cfg.plugins.index(), Some("file:///i.json"));
    }

    #[test]
    fn a_typo_is_an_error_with_its_place() {
        let dir = temp();
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("bad.toml");
        std::fs::write(&path, "[dictation]\nmode = \"hlod\"\n").expect("write");
        let Err(ConfigError::Parse { message, .. }) = load(&path) else {
            panic!("parsed a bad mode")
        };
        assert!(message.contains("line 2"), "{message}");
        std::fs::write(&path, "[dictaton]\nmode = \"hold\"\n").expect("write");
        assert!(
            matches!(load(&path), Err(ConfigError::Parse { .. })),
            "an unknown table is an error"
        );
    }

    #[test]
    fn a_save_reads_back_and_only_the_owner_can_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp();
        let path = dir.join("nested").join("config.toml");
        let mut cfg = Config::default();
        cfg.recognition.engines.insert(
            "doubao".into(),
            [("api_key".to_owned(), "k".to_owned())].into(),
        );
        cfg.overlay.scale = 1.5;
        save(&cfg, &path).expect("save");
        assert_eq!(load(&path).expect("load"), cfg);
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let left: Vec<_> = std::fs::read_dir(path.parent().expect("dir"))
            .expect("ls")
            .collect();
        assert_eq!(left.len(), 1, "no temp file left behind");
    }

    #[test]
    fn problems_name_what_cannot_be_used() {
        let mut cfg = Config::default();
        assert_eq!(cfg.problems(&["doubao"]), []);
        cfg.overlay.position = "middle".into();
        cfg.overlay.scale = 0.0;
        cfg.recognition.continuous = "nope".into();
        assert_eq!(
            cfg.problems(&["doubao"]),
            [
                Problem::UnknownPosition("middle".into()),
                Problem::BadScale,
                Problem::UnknownEngine(EngineRole::Continuous, "nope".into())
            ]
        );
    }

    #[test]
    fn home_is_expanded_only_at_the_start() {
        let Some(h) = home() else { return };
        assert_eq!(expand_home("~/a/b"), h.join("a/b"));
        assert_eq!(expand_home("~"), h);
        assert_eq!(expand_home("/x/~/y"), PathBuf::from("/x/~/y"));
        assert_eq!(expand_home("~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn plugin_hook_tables_parse_save_and_refuse_misspelt_keys() {
        let text = "[plugins]\norder = [\"replace\", \"polish\"]\n\n[plugins.limits]\ninline_ms = 30\nchain_ms = 3000\n\n[plugins.limits.polish]\ntransform_ms = 4000\n\n[plugins.settings.polish]\napi_key = \"k\"\n";
        let cfg = parse(Path::new("c.toml"), text).expect("parses");
        let p = &cfg.plugins;
        assert_eq!(p.order, ["replace", "polish"]);
        assert_eq!(p.limits.every.inline_ms, Some(30));
        assert_eq!(p.limits.chain_ms, Some(3000));
        assert_eq!(p.limits.of("polish").transform_ms, Some(4000));
        assert_eq!(p.limits.of("other"), Limits::default());
        assert_eq!(p.settings_of("polish")["api_key"], "k");
        assert!(p.settings_of("other").is_empty());

        let dir = temp();
        let path = dir.join("config.toml");
        save(&cfg, &path).expect("save");
        assert_eq!(load(&path).expect("load"), cfg);

        for bad in [
            "[plugins.limits]\ninline_mss = 3\n",
            "[plugins.limits.polish]\nchain_ms = 3\n",
            "[plugins.limits.polish]\nbogus = 3\n",
            "[plugins.limits]\nbogus = 3\n",
        ] {
            assert!(
                matches!(
                    parse(Path::new("c.toml"), bad),
                    Err(ConfigError::Parse { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn budgets_follow_the_users_word_then_the_packages_then_the_defaults() {
        let mut p = Plugins::default();
        let asked = Limits {
            transform_ms: Some(500),
            context_ms: Some(100),
            ..Limits::default()
        };
        assert_eq!(p.budgets("x", asked).transform, Duration::from_millis(500));
        assert_eq!(
            p.budgets("x", Limits::default()).transform,
            Duration::from_millis(2000)
        );
        p.limits.every.transform_ms = Some(900);
        assert_eq!(p.budgets("x", asked).transform, Duration::from_millis(900));
        p.limits.plugins.insert(
            "x".into(),
            Limits {
                transform_ms: Some(1), // below the range: clamped
                ..Limits::default()
            },
        );
        assert_eq!(p.budgets("x", asked).transform, Duration::from_millis(100));
        assert_eq!(p.budgets("x", asked).context, Duration::from_millis(100));
        assert_eq!(p.chain_budget(), Duration::from_millis(4000));
        p.limits.chain_ms = Some(99_999);
        assert_eq!(p.chain_budget(), Duration::from_millis(15_000));
    }

    #[test]
    fn the_order_ranks_listed_plugins_first() {
        let p = Plugins {
            order: vec!["b".into(), "a".into()],
            ..Plugins::default()
        };
        assert!(p.rank("b") < p.rank("a"));
        assert!(p.rank("a") < p.rank("z"));
        assert_eq!(p.rank("y"), p.rank("z"));
    }

    #[test]
    fn problems_name_plugin_ids_that_cannot_be_plugins() {
        let mut cfg = Config::default();
        cfg.plugins.order = vec!["ok".into(), "".into()];
        cfg.plugins
            .limits
            .plugins
            .insert(" x".into(), Limits::default());
        cfg.plugins.settings.insert("y ".into(), BTreeMap::new());
        assert_eq!(
            cfg.problems(&["doubao"]),
            [
                Problem::BadPluginId(PluginSection::Order, "".into()),
                Problem::BadPluginId(PluginSection::Limits, " x".into()),
                Problem::BadPluginId(PluginSection::Settings, "y ".into()),
            ]
        );
    }

    #[tokio::test]
    async fn watch_sees_atomic_saves_and_ignores_rewrites_of_the_same_text() {
        let dir = temp();
        let path = dir.join("config.toml");
        let mut changes = watch(path.clone());
        let mut cfg = Config::default();
        cfg.dictation.mode = Mode::Hold;
        save(&cfg, &path).expect("save");
        let got = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("in time");
        assert_eq!(
            got.expect("sent").expect("parsed").dictation.mode,
            Mode::Hold
        );

        save(&cfg, &path).expect("same text again");
        std::fs::write(&path, "[dictation]\nmode = 3\n").expect("broken");
        let got = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("in time");
        assert!(
            matches!(got, Some(Err(ConfigError::Parse { .. }))),
            "the rewrite of the same text sent nothing"
        );
    }
}
