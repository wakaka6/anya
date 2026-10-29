//! A scripted [`Control`] for the page's tests and `examples/web.rs`: it
//! records calls, keeps the config in memory and checks hotkeys with a small
//! grammar of its own.

// The example uses part of it, the tests another part.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anya_config::{Config, Problem};
use anya_core::audio::{Device, DeviceKind};
use anya_protocol::{ErrorCode, Field, FieldKind, Manifest, Source, Text};
use anya_runtime::{
    Control, Delivered, Dictation, EngineInfo, Entry, HotkeyRole, Info, Invalid, Notice, SaveError,
    Saved, Stats, Status, Subscription,
};

/// A running Anya that exists only in memory.
pub(crate) struct FakeControl {
    pub(crate) config: Mutex<Config>,
    pub(crate) status: Mutex<Status>,
    pub(crate) activity: Mutex<Vec<Entry>>,
    pub(crate) devices: Mutex<Result<Vec<Device>, String>>,
    pub(crate) engines: Vec<Manifest>,
    /// Answers every valid save with this error.
    pub(crate) save_error: Mutex<Option<SaveError>>,
    /// Reports this as the hotkeys failing to apply after a save.
    pub(crate) hotkeys_failed: Mutex<Option<String>>,
    /// What the OS recorder returns, after `record_delay`.
    pub(crate) record: Mutex<Result<Option<String>, String>>,
    pub(crate) record_delay: Mutex<Duration>,
    pub(crate) copy_error: Mutex<Option<String>>,
    /// Every call that changes something, in order.
    pub(crate) calls: Mutex<Vec<String>>,
    /// Every config saved.
    pub(crate) saved: Mutex<Vec<Config>>,
    pub(crate) copied: Mutex<Vec<String>>,
    pub(crate) quit: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl FakeControl {
    /// Two engines (`cloud` needs an API key, `whisper` needs nothing), one
    /// microphone, a few activity lines, English UI.
    pub(crate) fn sample() -> FakeControl {
        let mut config = Config::default();
        config.ui.language = "en".to_owned();
        config.recognition.engine = "cloud".to_owned();
        config.dictation.hotkeys = "F13".to_owned();
        let now = now_ms();
        FakeControl {
            config: Mutex::new(config),
            status: Mutex::new(Status {
                dictation: Dictation::Idle,
                finishing: 0,
                live_text: None,
                continuous_on: false,
                subscription: Subscription::Listening {
                    url: "ws://127.0.0.1:7590/asr".to_owned(),
                    subscribers: 2,
                },
                stats: Stats {
                    sessions: 42,
                    chars: 5_300,
                    audio_ms: 1_060_000,
                    last_chars: 120,
                    last_audio_ms: 24_000,
                },
            }),
            activity: Mutex::new(vec![
                Entry::Notice {
                    at: now - 600_000,
                    notice: Notice::ConfigReloaded,
                    detail: None,
                },
                Entry::Final {
                    at: now - 300_000,
                    source: Source::Dictation,
                    text: "Meet at the station at nine.".to_owned(),
                    delivered: Delivered::Pasted,
                },
                Entry::Failed {
                    at: now - 200_000,
                    source: Source::Dictation,
                    error: ErrorCode::Connect,
                    detail: Some("connection refused".to_owned()),
                },
                Entry::Final {
                    at: now - 100_000,
                    source: Source::Continuous,
                    text: "The quarterly numbers look good.".to_owned(),
                    delivered: Delivered::Kept,
                },
            ]),
            devices: Mutex::new(Ok(vec![Device {
                id: "BuiltInMic".to_owned(),
                name: "MacBook Pro Microphone".to_owned(),
                kind: DeviceKind::Microphone,
            }])),
            engines: vec![cloud(), whisper()],
            save_error: Mutex::new(None),
            hotkeys_failed: Mutex::new(None),
            record: Mutex::new(Ok(Some("F9".to_owned()))),
            record_delay: Mutex::new(Duration::ZERO),
            copy_error: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            saved: Mutex::new(Vec::new()),
            copied: Mutex::new(Vec::new()),
            quit: AtomicBool::new(false),
        }
    }

    /// The calls so far.
    pub(crate) fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }

    fn call(&self, what: String) {
        lock(&self.calls).push(what);
    }

    fn notice(&self, notice: Notice) {
        lock(&self.activity).push(Entry::Notice {
            at: now_ms(),
            notice,
            detail: None,
        });
    }

    fn ready(&self, id: &str, settings: &BTreeMap<String, String>) -> bool {
        self.engines.iter().find(|m| m.id == id).is_some_and(|m| {
            m.fields
                .iter()
                .filter(|f| f.required)
                .all(|f| settings.get(&f.key).is_some_and(|v| !v.trim().is_empty()))
        })
    }

    fn validate(&self, config: &Config) -> Vec<Invalid> {
        let mut out = Vec::new();
        if let Err(e) = check(&config.dictation.hotkeys, HotkeyRole::Dictation, "") {
            out.push(e);
        }
        if let Err(e) = check(
            &config.continuous.hotkey,
            HotkeyRole::Continuous,
            &config.dictation.hotkeys,
        ) {
            out.push(e);
        }
        let ids: Vec<&str> = self.engines.iter().map(|m| m.id.as_str()).collect();
        for problem in config.problems(&ids) {
            out.push(match problem {
                Problem::UnknownPosition(value) => Invalid::UnknownPosition { value },
                Problem::BadScale => Invalid::BadScale,
                Problem::UnknownEngine(id) => Invalid::UnknownEngine { id },
            });
        }
        let address = config.subscription.address();
        let loopback = ["127.0.0.1:", "localhost:", "[::1]:", "unix:"]
            .iter()
            .any(|p| address.starts_with(p));
        if !loopback {
            out.push(Invalid::BadAddress {
                address: address.to_owned(),
                detail: "not a loopback address".to_owned(),
            });
        }
        out
    }

    fn saved_reply(&self) -> Saved {
        Saved {
            hotkeys_failed: lock(&self.hotkeys_failed).clone(),
        }
    }
}

impl Control for FakeControl {
    fn info(&self) -> Info {
        Info {
            platform: if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            },
            desktop: if cfg!(target_os = "macos") {
                "darwin"
            } else {
                "wayland"
            }
            .to_owned(),
            version: "0.0.0".to_owned(),
            config_path: PathBuf::from("/home/me/.config/anya/config.toml"),
            debug: false,
        }
    }

    fn config(&self) -> Config {
        lock(&self.config).clone()
    }

    fn save(&self, config: Config) -> Result<Saved, SaveError> {
        self.call("save".to_owned());
        let invalid = self.validate(&config);
        if !invalid.is_empty() {
            return Err(SaveError::Invalid(invalid));
        }
        if let Some(e) = lock(&self.save_error).clone() {
            return Err(e);
        }
        let mut current = lock(&self.config);
        if current.continuous.enabled != config.continuous.enabled {
            lock(&self.status).continuous_on = config.continuous.enabled;
        }
        *current = config.clone();
        lock(&self.saved).push(config);
        Ok(self.saved_reply())
    }

    fn check_hotkeys(&self, value: &str, role: HotkeyRole) -> Result<String, Invalid> {
        let dictation = lock(&self.config).dictation.hotkeys.clone();
        check(value, role, &dictation)
    }

    fn engines(&self) -> Vec<EngineInfo> {
        let config = self.config();
        self.engines
            .iter()
            .map(|m| EngineInfo {
                manifest: m.clone(),
                ready: self.ready(&m.id, &config.recognition.settings(&m.id)),
            })
            .collect()
    }

    fn engine_ready(&self, id: &str, settings: &BTreeMap<String, String>) -> bool {
        self.ready(id, settings)
    }

    fn devices(&self) -> Result<Vec<Device>, String> {
        lock(&self.devices).clone()
    }

    fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    fn activity(&self) -> Vec<Entry> {
        lock(&self.activity).clone()
    }

    fn set_dictation(&self, on: bool) -> Result<Saved, SaveError> {
        self.call(format!("set_dictation {on}"));
        if let Some(e) = lock(&self.save_error).clone() {
            return Err(e);
        }
        lock(&self.config).dictation.enabled = on;
        self.notice(if on {
            Notice::DictationOn
        } else {
            Notice::DictationOff
        });
        Ok(self.saved_reply())
    }

    fn set_continuous(&self, on: bool) -> Result<Saved, SaveError> {
        self.call(format!("set_continuous {on}"));
        if let Some(e) = lock(&self.save_error).clone() {
            return Err(e);
        }
        lock(&self.config).continuous.enabled = on;
        lock(&self.status).continuous_on = on;
        self.notice(if on {
            Notice::ContinuousOn
        } else {
            Notice::ContinuousOff
        });
        Ok(self.saved_reply())
    }

    fn suspend_hotkeys(&self, period: Option<Duration>) {
        self.call(format!("suspend_hotkeys {period:?}"));
    }

    fn record_hotkey(&self, timeout: Duration) -> Result<Option<String>, String> {
        self.call(format!("record_hotkey {timeout:?}"));
        std::thread::sleep(*lock(&self.record_delay));
        lock(&self.record).clone()
    }

    fn copy(&self, text: &str) -> Result<(), String> {
        if let Some(e) = lock(&self.copy_error).clone() {
            return Err(e);
        }
        lock(&self.copied).push(text.to_owned());
        Ok(())
    }

    fn quit(&self) {
        self.quit.store(true, Ordering::SeqCst);
    }
}

fn field(
    key: &str,
    label: Text,
    help: Text,
    kind: FieldKind,
    required: bool,
    advanced: bool,
) -> Field {
    Field {
        key: key.to_owned(),
        label,
        help,
        kind,
        required,
        advanced,
    }
}

fn cloud() -> Manifest {
    Manifest {
        id: "cloud".to_owned(),
        title: Text::localized([("en", "Cloud Stream"), ("zh-CN", "云端流式")]),
        blurb: Text::localized([
            ("en", "Streaming recognition: text appears while you speak"),
            ("zh-CN", "流式识别，边说边出字"),
        ]),
        docs: Some("https://example.com/cloud-docs".to_owned()),
        streaming: true,
        fields: vec![
            field(
                "api_key",
                "API Key".into(),
                Text::localized([("en", "Required"), ("zh-CN", "必填")]),
                FieldKind::Secret,
                true,
                false,
            ),
            field(
                "model",
                Text::localized([("en", "Model"), ("zh-CN", "模型")]),
                Text::default(),
                FieldKind::Choice {
                    options: vec!["v1".to_owned(), "v2".to_owned()],
                },
                false,
                true,
            ),
            field(
                "region",
                Text::localized([("en", "Region"), ("zh-CN", "区域")]),
                Text::default(),
                FieldKind::Text,
                false,
                true,
            ),
        ],
    }
}

fn whisper() -> Manifest {
    Manifest {
        id: "whisper".to_owned(),
        title: "Whisper".into(),
        blurb: Text::localized([
            ("en", "Recognizes the whole recording when you stop"),
            ("zh-CN", "整段识别"),
        ]),
        docs: None,
        streaming: false,
        fields: vec![field(
            "model",
            Text::localized([("en", "Model"), ("zh-CN", "模型")]),
            Text::default(),
            FieldKind::Text,
            false,
            true,
        )],
    }
}

/// A hotkey list in canonical names, or why it cannot be used for `role`.
/// `dictation` is the dictation list a continuous hotkey must not reuse.
fn check(value: &str, role: HotkeyRole, dictation: &str) -> Result<String, Invalid> {
    let mut combos = Vec::new();
    for part in value
        .split([',', '，'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let combo = combo(part).ok_or_else(|| Invalid::HotkeyUnparsable {
            value: value.to_owned(),
        })?;
        if !combos.contains(&combo) {
            combos.push(combo);
        }
    }
    for combo in &combos {
        let fn_held = combo.split('+').any(|p| p == "Fn");
        let key = combo.rsplit('+').next().unwrap_or("");
        if fn_held && is_function_key(key) {
            return Err(Invalid::HotkeyFnFunctionKey {
                combo: combo.clone(),
            });
        }
        if !fn_held && is_text_key(key) {
            return Err(Invalid::HotkeyTextKey {
                combo: combo.clone(),
            });
        }
        if role == HotkeyRole::Continuous
            && check(dictation, HotkeyRole::Dictation, "")
                .is_ok_and(|d| d.split(", ").any(|c| c == combo))
        {
            return Err(Invalid::HotkeyTaken {
                combo: combo.clone(),
            });
        }
    }
    Ok(combos.join(", "))
}

const MODS: [&str; 5] = ["Ctrl", "Alt", "Shift", "Super", "Fn"];

fn combo(part: &str) -> Option<String> {
    let mut mods = [false; 5];
    let mut key = None;
    for name in part.split('+').map(str::trim) {
        let lower = name.to_ascii_lowercase();
        let m = match lower.as_str() {
            "ctrl" | "control" => Some(0),
            "alt" | "option" => Some(1),
            "shift" => Some(2),
            "super" | "cmd" | "command" | "win" | "meta" => Some(3),
            "fn" | "function" => Some(4),
            _ => None,
        };
        match m {
            Some(i) => mods[i] = true,
            None if key.is_none() => key = Some(key_name(&lower)?),
            None => return None,
        }
    }
    let mut names: Vec<String> = MODS
        .iter()
        .zip(mods)
        .filter(|(_, held)| *held)
        .map(|(n, _)| (*n).to_owned())
        .collect();
    names.extend(key);
    (!names.is_empty()).then(|| names.join("+"))
}

fn key_name(lower: &str) -> Option<String> {
    let named = [
        ("tab", "Tab"),
        ("space", "Space"),
        ("enter", "Enter"),
        ("return", "Enter"),
        ("esc", "Escape"),
        ("escape", "Escape"),
        ("backspace", "Backspace"),
        ("capslock", "CapsLock"),
        ("up", "Up"),
        ("down", "Down"),
        ("left", "Left"),
        ("right", "Right"),
        ("home", "Home"),
        ("end", "End"),
        ("pageup", "PageUp"),
        ("pagedown", "PageDown"),
        ("insert", "Insert"),
        ("delete", "Delete"),
        ("menu", "Menu"),
    ];
    if let Some((_, n)) = named.iter().find(|(k, _)| *k == lower) {
        return Some((*n).to_owned());
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (1..=24).contains(&n)
    {
        return Some(format!("F{n}"));
    }
    if let Some(n) = lower
        .strip_prefix("num")
        .filter(|n| n.len() == 1 && n.chars().all(|c| c.is_ascii_digit()))
    {
        return Some(format!("Num{n}"));
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() || "`-=[]\\;',./".contains(c) => {
            Some(c.to_ascii_uppercase().to_string())
        }
        _ => None,
    }
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn is_text_key(key: &str) -> bool {
    key == "Space" || key.chars().count() == 1
}
