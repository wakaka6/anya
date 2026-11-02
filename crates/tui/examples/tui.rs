//! The terminal UI against a pretend Anya: two engines, a microphone and a
//! loopback device, a dictation session every few seconds. Saves stay in
//! memory.
//!
//! ```bash
//! cargo run -p anya-tui --example tui            # the system's language
//! cargo run -p anya-tui --example tui -- zh-CN
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anya_config::Config;
use anya_core::audio::{Device, DeviceKind};
use anya_i18n::Catalog;
use anya_protocol::{Field, FieldKind, Manifest, Scope, Source, Text};
use anya_runtime::{
    BuiltinHooks, ClientIssue, Control, Delivered, Dictation, EngineInfo, Entry, HookInfo,
    HotkeyRole, Info, InstalledPlugin, Invalid, Lan, Market, Notice, PairError, PairedClient,
    Pairing, PluginIssue, PreviewIssue, PreviewView, SaveError, Saved, Stats, Status, Subscription,
};

struct Pretend {
    started: Instant,
    config: Mutex<Config>,
    activity: Mutex<Vec<Entry>>,
    continuous: Mutex<bool>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn field(key: &str, en: &str, zh: &str, kind: FieldKind, required: bool) -> Field {
    Field {
        key: key.to_owned(),
        label: Text::localized([("en", en), ("zh-CN", zh)]),
        help: Text::localized([("en", "Shown under the field"), ("zh-CN", "显示在字段下方")]),
        kind,
        required,
        advanced: false,
    }
}

impl Pretend {
    fn new() -> Pretend {
        let mut config = Config::default();
        for role in anya_config::EngineRole::ALL {
            config.recognition.set_engine(role, "stream");
        }
        config.recognition.engines.insert(
            "stream".to_owned(),
            [("api_key".to_owned(), "sk-pretend-0123456789".to_owned())].into(),
        );
        Pretend {
            started: Instant::now(),
            config: Mutex::new(config),
            activity: Mutex::new(vec![Entry::Notice {
                at: now_ms(),
                notice: Notice::DictationOn,
                detail: None,
            }]),
            continuous: Mutex::new(false),
        }
    }
}

impl Control for Pretend {
    fn info(&self) -> Info {
        Info {
            platform: if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            },
            desktop: "pretend".to_owned(),
            version: "0.0.0".to_owned(),
            config_path: PathBuf::from("/tmp/anya-example/config.toml"),
            hotwords_path: PathBuf::from("/tmp/anya-example/hotwords.txt"),
            replacements_path: PathBuf::from("/tmp/anya-example/replacements.txt"),
        }
    }

    fn config(&self) -> Config {
        self.config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn save(&self, config: Config) -> Result<Saved, SaveError> {
        if !anya_config::POSITIONS.contains(&config.overlay.position.as_str()) {
            return Err(SaveError::Invalid(vec![Invalid::UnknownPosition {
                value: config.overlay.position,
            }]));
        }
        *self.config.lock().unwrap_or_else(PoisonError::into_inner) = config;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn check_hotkeys(&self, value: &str, _role: HotkeyRole) -> Result<String, Invalid> {
        let mut out = Vec::new();
        for combo in value.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            let last = combo.rsplit('+').next().unwrap_or(combo);
            if last.chars().count() == 1 && last.chars().all(char::is_alphanumeric) {
                return Err(Invalid::HotkeyTextKey {
                    combo: combo.to_owned(),
                });
            }
            out.push(combo.to_owned());
        }
        Ok(out.join(", "))
    }

    fn engines(&self) -> Vec<EngineInfo> {
        let stream = Manifest {
            id: "stream".to_owned(),
            title: Text::localized([("en", "Pretend Stream"), ("zh-CN", "示例流式")]),
            blurb: Text::localized([
                ("en", "Text appears while you speak"),
                ("zh-CN", "边说边出字"),
            ]),
            docs: Some("https://example.com/stream".to_owned()),
            streaming: true,
            fields: vec![
                field("api_key", "API Key", "API Key", FieldKind::Secret, true),
                field("model", "Model", "模型", FieldKind::Text, false),
                field(
                    "rate",
                    "Sample rate",
                    "采样率",
                    FieldKind::Choice {
                        options: vec!["16000".to_owned(), "8000".to_owned()],
                    },
                    false,
                ),
            ],
        };
        let batch = Manifest {
            id: "batch".to_owned(),
            title: Text::localized([("en", "Pretend Batch"), ("zh-CN", "示例整段")]),
            blurb: Text::localized([
                ("en", "One result after you stop"),
                ("zh-CN", "说完后一次返回"),
            ]),
            docs: None,
            streaming: false,
            fields: vec![field(
                "api_key",
                "API Key",
                "API Key",
                FieldKind::Secret,
                true,
            )],
        };
        let config = self.config();
        [stream, batch]
            .into_iter()
            .map(|m| EngineInfo {
                ready: self.engine_ready(&m.id, &config.recognition.settings(&m.id)),
                enabled: None,
                manifest: m,
            })
            .collect()
    }

    fn engine_ready(&self, _id: &str, settings: &BTreeMap<String, String>) -> bool {
        settings
            .get("api_key")
            .is_some_and(|k| !k.trim().is_empty())
    }

    fn devices(&self) -> Result<Vec<Device>, String> {
        Ok(vec![
            Device {
                id: "builtin".to_owned(),
                name: "Built-in Microphone".to_owned(),
                kind: DeviceKind::Microphone,
            },
            Device {
                id: "blackhole".to_owned(),
                name: "BlackHole 2ch".to_owned(),
                kind: DeviceKind::Loopback,
            },
        ])
    }

    fn status(&self) -> Status {
        // A pretend session every 8 s: 3 s recording, 1 s finishing.
        let phase = self.started.elapsed().as_millis() % 8000;
        let (dictation, finishing, live) = match phase {
            0..3000 => (
                Dictation::Recording,
                0,
                Some("hello from the pretend engine".to_owned()),
            ),
            3000..4000 => (Dictation::Idle, 1, None),
            _ => (Dictation::Idle, 0, None),
        };
        let mut activity = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
        let sessions = self.started.elapsed().as_millis() / 8000;
        let finals = activity
            .iter()
            .filter(|e| matches!(e, Entry::Final { .. }))
            .count();
        if phase >= 4000 && u128::try_from(finals).unwrap_or(0) <= sessions {
            activity.push(Entry::Final {
                at: now_ms(),
                source: Source::Dictation,
                text: "Hello from the pretend engine.".to_owned(),
                delivered: Delivered::Pasted,
                session: None,
                engine: None,
            });
        }
        let done = u64::try_from(finals).unwrap_or(0);
        Status {
            dictation,
            finishing,
            live_text: live,
            level: None,
            continuous_on: *self
                .continuous
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            subscription: Subscription::Listening {
                url: "ws://127.0.0.1:7590/".to_owned(),
                subscribers: 1,
            },
            lan: Lan::Off,
            stats: Stats {
                sessions: done,
                chars: done * 30,
                audio_ms: done * 3000,
                last_chars: 30,
                last_audio_ms: 3000,
            },
        }
    }

    fn clients(&self) -> Vec<PairedClient> {
        Vec::new()
    }

    fn start_pairing(&self) -> Result<Pairing, PairError> {
        Err(PairError::LanOff)
    }

    fn set_client_scopes(&self, _id: &str, _scopes: Vec<Scope>) -> Result<(), ClientIssue> {
        Err(ClientIssue::Write {
            detail: "no paired clients".to_owned(),
        })
    }

    fn remove_client(&self, _id: &str) -> Result<(), ClientIssue> {
        Err(ClientIssue::Write {
            detail: "no paired clients".to_owned(),
        })
    }

    fn hooks(&self) -> Vec<HookInfo> {
        Vec::new()
    }

    fn builtin_hooks(&self) -> Vec<BuiltinHooks> {
        Vec::new()
    }

    fn plugins(&self) -> Vec<InstalledPlugin> {
        Vec::new()
    }

    fn plugin_market(&self) -> Market {
        Market {
            plugins: Vec::new(),
            unreachable: Some(PluginIssue::Bun {
                reason: "no registry".to_owned(),
            }),
        }
    }

    fn plugin_icon(&self, _name: &str) -> Option<anya_runtime::PluginIcon> {
        None
    }

    fn install_plugin(&self, _id: &str) -> Result<(), PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no registry".to_owned(),
        })
    }

    fn set_plugin_enabled(&self, _id: &str, _on: bool) -> Result<(), PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no plugins".to_owned(),
        })
    }

    fn remove_plugin(&self, _id: &str) -> Result<(), PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no plugins".to_owned(),
        })
    }

    fn activity(&self) -> Vec<Entry> {
        self.activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_dictation(&self, on: bool) -> Result<Saved, SaveError> {
        self.config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dictation
            .enabled = on;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn set_continuous(&self, on: bool) -> Result<Saved, SaveError> {
        *self
            .continuous
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = on;
        self.config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .continuous
            .enabled = on;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn suspend_hotkeys(&self, _period: Option<Duration>) {}

    fn record_hotkey(&self, timeout: Duration) -> Result<Option<String>, String> {
        std::thread::sleep(timeout.min(Duration::from_secs(2)));
        Ok(Some("Alt+F8".to_owned()))
    }

    fn preview_overlay(&self, _view: PreviewView) -> Result<(), PreviewIssue> {
        Ok(())
    }

    fn retry_dictation(&self) {}

    fn dismiss_dictation(&self) {}

    fn copy(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn quit(&self) {}
}

fn main() -> Result<(), anya_tui::TuiError> {
    let language = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "system".to_owned());
    anya_tui::run(Arc::new(Pretend::new()), Catalog::for_setting(&language))
}
