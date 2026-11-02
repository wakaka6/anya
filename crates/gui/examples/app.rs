//! The tray app against a fake Anya: `cargo run -p anya-gui --example app`.
//! Click the tray icon for the panel; the settings window opens at start.
//! Saves change the fake's config in memory only. Quit from the tray menu.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anya_config::{Config, EngineRole};
use anya_core::audio::{Device, DeviceKind};
use anya_i18n::Catalog;
use anya_protocol::{Manifest, Scope, Source, Text};
use anya_runtime::{
    BuiltinHooks, ClientIssue, Control, Delivered, Dictation, EngineInfo, Entry, HookInfo,
    HotkeyRole, Info, InstalledPlugin, Invalid, Lan, MarketPlugin, PairError, PairedClient,
    Pairing, PluginIssue, PreviewIssue, PreviewView, SaveError, Saved, Stats, Status, Subscription,
};

struct Fake {
    config: Mutex<Config>,
    continuous_on: AtomicBool,
    quit: tokio::sync::watch::Sender<bool>,
}

impl Fake {
    fn config(&self) -> std::sync::MutexGuard<'_, Config> {
        self.config.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Control for Fake {
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
            version: env!("CARGO_PKG_VERSION").to_owned(),
            config_path: PathBuf::from("~/.config/anya/config.toml"),
            hotwords_path: PathBuf::from("~/.config/anya/hotwords.txt"),
            replacements_path: PathBuf::from("~/.config/anya/replacements.txt"),
        }
    }

    fn config(&self) -> Config {
        self.config().clone()
    }

    fn save(&self, config: Config) -> Result<Saved, SaveError> {
        *self.config() = config;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn check_hotkeys(&self, value: &str, _role: HotkeyRole) -> Result<String, Invalid> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() == 1 {
            return Err(Invalid::HotkeyUnparsable {
                value: value.to_owned(),
            });
        }
        Ok(value.to_owned())
    }

    fn engines(&self) -> Vec<EngineInfo> {
        vec![EngineInfo {
            manifest: Manifest {
                id: "demo".to_owned(),
                title: Text::localized([("en", "Demo engine"), ("zh-CN", "示例引擎")]),
                blurb: Text::from("A fake engine for this example."),
                docs: None,
                streaming: true,
                fields: Vec::new(),
            },
            ready: true,
            enabled: None,
        }]
    }

    fn engine_ready(&self, _id: &str, _settings: &BTreeMap<String, String>) -> bool {
        true
    }

    fn devices(&self) -> Result<Vec<Device>, String> {
        Ok(vec![
            Device {
                id: "mic".to_owned(),
                name: "MacBook Pro Microphone".to_owned(),
                kind: DeviceKind::Microphone,
            },
            Device {
                id: "loop".to_owned(),
                name: "BlackHole 2ch".to_owned(),
                kind: DeviceKind::Loopback,
            },
        ])
    }

    fn status(&self) -> Status {
        Status {
            dictation: Dictation::Idle,
            finishing: 0,
            live_text: None,
            level: None,
            continuous_on: self.continuous_on.load(Ordering::Relaxed),
            subscription: Subscription::Listening {
                url: "ws://127.0.0.1:7590/asr".to_owned(),
                subscribers: 1,
            },
            lan: Lan::Off,
            stats: Stats {
                sessions: 42,
                chars: 3180,
                audio_ms: 900_000,
                last_chars: 36,
                last_audio_ms: 9_000,
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

    fn plugin_market(&self) -> Result<Vec<MarketPlugin>, PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no registry".to_owned(),
        })
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
        vec![Entry::Final {
            at: now_ms(),
            source: Source::Dictation,
            text: "Anya hears what you mean.".to_owned(),
            delivered: Delivered::Pasted,
            session: None,
            engine: None,
        }]
    }

    fn set_dictation(&self, on: bool) -> Result<Saved, SaveError> {
        self.config().dictation.enabled = on;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn set_continuous(&self, on: bool) -> Result<Saved, SaveError> {
        self.config().continuous.enabled = on;
        self.continuous_on.store(on, Ordering::Relaxed);
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn suspend_hotkeys(&self, _period: Option<Duration>) {}

    fn record_hotkey(&self, timeout: Duration) -> Result<Option<String>, String> {
        std::thread::sleep(timeout.min(Duration::from_secs(1)));
        Ok(Some("Alt+Super".to_owned()))
    }

    fn preview_overlay(&self, _view: PreviewView) -> Result<(), PreviewIssue> {
        Ok(())
    }

    fn retry_dictation(&self) {}

    fn dismiss_dictation(&self) {}

    fn copy(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn quit(&self) {
        self.quit.send_replace(true);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (quit_tx, quit_rx) = tokio::sync::watch::channel(false);
    let mut config = Config::default();
    for role in EngineRole::ALL {
        config.recognition.set_engine(role, "demo");
    }
    let catalog = Catalog::for_setting(&config.ui.language);
    let control = Arc::new(Fake {
        config: Mutex::new(config),
        continuous_on: AtomicBool::new(false),
        quit: quit_tx,
    });
    let config_path = std::env::temp_dir().join("anya-example-config.toml");
    if anya_gui::desktop::activate_existing(&config_path)? {
        // The example already runs; it was asked to show its settings.
        return Ok(());
    }
    anya_gui::desktop::run(control, catalog, runtime.handle().clone(), quit_rx)?;
    Ok(())
}
