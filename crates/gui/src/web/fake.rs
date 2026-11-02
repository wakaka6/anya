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

use anya_config::{Config, EngineRole, Problem};
use anya_core::audio::{Device, DeviceKind};
use anya_core::hooks::Runs;
use anya_protocol::{
    ErrorCode, Field, FieldKind, Manifest, PairingOffer, Provide, Scope, Source, Text,
};
use anya_runtime::{
    BuiltinHooks, ClientIssue, Control, Delivered, Dictation, EngineInfo, Entry, HookInfo,
    HotkeyRole, Info, InstalledPlugin, Invalid, Lan, Market, MarketPlugin, Notice, PairError,
    PairedClient, Pairing, PluginIssue, PluginLoad, PreviewIssue, PreviewView, Route, SaveError,
    Saved, Stats, Status, Subscription, UpdateIssue, UpdateState,
};

/// A running Anya that exists only in memory.
pub(crate) struct FakeControl {
    pub(crate) config: Mutex<Config>,
    pub(crate) status: Mutex<Status>,
    pub(crate) activity: Mutex<Vec<Entry>>,
    /// What the route page shows; the tests and the example set it.
    pub(crate) route: Mutex<Route>,
    /// Paired apps; pairing adds none, tests and the example put them here.
    pub(crate) clients: Mutex<Vec<PairedClient>>,
    /// Fails every client change with this write error.
    pub(crate) clients_write_error: Mutex<Option<String>>,
    /// Installed plugins; installing from `market` adds one, disabled.
    pub(crate) plugins: Mutex<Vec<InstalledPlugin>>,
    pub(crate) market: Mutex<Market>,
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
    /// Refuses every overlay preview with this.
    pub(crate) preview_error: Mutex<Option<PreviewIssue>>,
    /// What the update check knows; an upgrade that succeeds moves
    /// `Available` to `Restart`.
    pub(crate) update: Mutex<UpdateState>,
    /// What `upgrade` answers.
    pub(crate) upgrade: Mutex<Result<(), UpdateIssue>>,
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
    /// Three engines (`cloud` needs an API key, `whisper` and the plugin's
    /// `sherpa` need nothing), one microphone, a few activity lines, English UI.
    pub(crate) fn sample() -> FakeControl {
        let mut config = Config::default();
        config.ui.language = "en".to_owned();
        for role in EngineRole::ALL {
            config.recognition.set_engine(role, "cloud");
        }
        config.dictation.hotkeys = "F13".to_owned();
        let now = now_ms();
        FakeControl {
            config: Mutex::new(config),
            status: Mutex::new(Status {
                dictation: Dictation::Idle,
                finishing: 0,
                live_text: None,
                level: None,
                continuous_on: false,
                subscription: Subscription::Listening {
                    url: "ws://127.0.0.1:7590/asr".to_owned(),
                    subscribers: 2,
                },
                lan: Lan::Off,
                stats: Stats {
                    sessions: 128,
                    chars: 5_320,
                    audio_ms: 2_460_000,
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
                    session: Some(anya_protocol::SessionId("anya-1-1".to_owned())),
                    engine: Some("cloud".to_owned()),
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
                    session: None,
                    engine: None,
                },
            ]),
            route: Mutex::new(Route::default()),
            clients: Mutex::new(Vec::new()),
            clients_write_error: Mutex::new(None),
            plugins: Mutex::new(Vec::new()),
            market: Mutex::new(Market {
                plugins: vec![
                    MarketPlugin {
                        id: "echo".to_owned(),
                        name: Text::localized([("en", "Echo"), ("zh-CN", "回声")]),
                        description: Text::localized([
                            ("en", "Answers with byte counts"),
                            ("zh-CN", "回报字节数"),
                        ]),
                        version: "1.0.0".to_owned(),
                        official: false,
                        streaming: true,
                        provides: vec![Provide::Engine],
                        host: true,
                        permissions: vec!["network:example.com".to_owned()],
                    },
                    MarketPlugin {
                        id: "whisper-local".to_owned(),
                        name: Text::localized([("en", "Local Whisper"), ("zh-CN", "本地 Whisper")]),
                        description: Text::localized([
                            ("en", "Recognizes on this computer; audio never leaves it"),
                            ("zh-CN", "在本机识别，音频不会离开这台电脑"),
                        ]),
                        version: "0.4.2".to_owned(),
                        official: false,
                        streaming: false,
                        provides: vec![Provide::Engine],
                        host: true,
                        permissions: Vec::new(),
                    },
                    MarketPlugin {
                        id: "deepgram".to_owned(),
                        name: "Deepgram".into(),
                        description: Text::localized([
                            ("en", "Streaming recognition in English and more"),
                            ("zh-CN", "流式识别，支持英语等多种语言"),
                        ]),
                        version: "2.1.0".to_owned(),
                        official: false,
                        streaming: true,
                        provides: vec![Provide::Engine],
                        host: true,
                        permissions: vec!["network:api.deepgram.com".to_owned()],
                    },
                    MarketPlugin {
                        id: "polish".to_owned(),
                        name: "Polish".into(),
                        description: Text::localized([
                            ("en", "Tidies punctuation and casing with a language model"),
                            ("zh-CN", "用语言模型整理标点和大小写"),
                        ]),
                        version: "0.3.0".to_owned(),
                        official: false,
                        streaming: false,
                        provides: vec![Provide::Transform],
                        host: true,
                        permissions: vec!["network:api.openai.com".to_owned()],
                    },
                    MarketPlugin {
                        id: "swap".to_owned(),
                        name: "Swap".into(),
                        description: Text::localized([
                            ("en", "Rewrites a few spoken phrases into symbols"),
                            ("zh-CN", "把几个口述短语改写成符号"),
                        ]),
                        version: "1.2.0".to_owned(),
                        official: true,
                        streaming: false,
                        provides: vec![Provide::Context, Provide::Transform],
                        host: false,
                        permissions: Vec::new(),
                    },
                ],
                unreachable: None,
            }),
            devices: Mutex::new(Ok(vec![Device {
                id: "BuiltInMic".to_owned(),
                name: "MacBook Pro Microphone".to_owned(),
                kind: DeviceKind::Microphone,
            }])),
            engines: vec![cloud(), whisper(), sherpa()],
            save_error: Mutex::new(None),
            hotkeys_failed: Mutex::new(None),
            record: Mutex::new(Ok(Some("F9".to_owned()))),
            record_delay: Mutex::new(Duration::ZERO),
            copy_error: Mutex::new(None),
            preview_error: Mutex::new(None),
            update: Mutex::new(UpdateState::Unknown),
            upgrade: Mutex::new(Ok(())),
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
                Problem::UnknownEngine(role, id) => Invalid::UnknownEngine { role, id },
                Problem::BadPluginId(_, id) => Invalid::BadPluginId { id },
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
            hotwords_path: PathBuf::from("/home/me/.config/anya/hotwords.txt"),
            replacements_path: PathBuf::from("/home/me/.config/anya/replacements.txt"),
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
                // `cloud` and `whisper` stand for built-in engines with a switch; `sherpa`
                // for a plugin's, which has its switch on the Plugins page.
                enabled: (m.id != "sherpa")
                    .then(|| config.plugins.builtin.get(&m.id) != Some(&false)),
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

    fn route(&self) -> Route {
        lock(&self.route).clone()
    }

    fn plugins(&self) -> Vec<InstalledPlugin> {
        let config = lock(&self.config).plugins.clone();
        let mut plugins = lock(&self.plugins).clone();
        for p in &mut plugins {
            let required: Vec<String> = p
                .fields
                .iter()
                .filter(|f| f.required)
                .map(|f| f.key.clone())
                .collect();
            p.missing = config.unset(&p.id, &required);
        }
        plugins
    }

    fn hooks(&self) -> Vec<HookInfo> {
        let config = lock(&self.config).clone();
        let on = |id: &str| config.plugins.builtin.get(id).copied().unwrap_or(true);
        let plugins = lock(&self.plugins).clone();
        let mut movable: Vec<HookInfo> = Vec::new();
        if on("replace") {
            movable.push(HookInfo {
                plugin: "replace".to_owned(),
                hook: Provide::Transform,
                runs: Runs::Builtin,
            });
        }
        let mut actions = Vec::new();
        if on("community-hooks") {
            for p in plugins.iter().filter(|p| p.enabled) {
                for hook in &p.provides {
                    let info = HookInfo {
                        plugin: p.id.clone(),
                        hook: *hook,
                        runs: if p.host { Runs::Host } else { Runs::Inline },
                    };
                    match hook {
                        Provide::Context | Provide::Transform => movable.push(info),
                        Provide::Action => actions.push(info),
                        _ => {}
                    }
                }
            }
        }
        // Listed ids first, in the list's order; the rest by id.
        movable.sort_by_key(
            |h| match config.plugins.order.iter().position(|id| *id == h.plugin) {
                Some(i) => (0, i, String::new()),
                None => (1, 0, h.plugin.clone()),
            },
        );
        movable.extend(actions);
        movable
    }

    fn builtin_hooks(&self) -> Vec<BuiltinHooks> {
        let config = lock(&self.config).clone();
        let on = |id: &str| config.plugins.builtin.get(id).copied().unwrap_or(true);
        vec![
            BuiltinHooks {
                id: "replace".to_owned(),
                provides: vec![Provide::Transform],
                on: on("replace"),
            },
            BuiltinHooks {
                id: "community-hooks".to_owned(),
                provides: vec![Provide::Context, Provide::Transform, Provide::Action],
                on: on("community-hooks"),
            },
        ]
    }

    fn plugin_market(&self) -> Market {
        self.call("plugin_market".to_owned());
        lock(&self.market).clone()
    }

    fn install_plugin(&self, id: &str) -> Result<(), PluginIssue> {
        self.call(format!("install_plugin {id}"));
        let release = lock(&self.market)
            .plugins
            .clone()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| PluginIssue::NotInIndex {
                id: id.to_owned(),
                version: "latest".to_owned(),
            })?;
        let mut plugins = lock(&self.plugins);
        plugins.retain(|p| p.id != id);
        plugins.push(InstalledPlugin {
            id: id.to_owned(),
            version: release.version,
            enabled: false,
            load: PluginLoad::Off,
            title: Some(release.name),
            description: Some(release.description),
            provides: release.provides,
            host: release.host,
            permissions: release.permissions,
            fields: Vec::new(),
            missing: Vec::new(),
        });
        Ok(())
    }

    fn set_plugin_enabled(&self, id: &str, on: bool) -> Result<(), PluginIssue> {
        self.call(format!("set_plugin_enabled {id} {on}"));
        let mut plugins = lock(&self.plugins);
        let plugin = plugins
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| PluginIssue::NotInstalled { id: id.to_owned() })?;
        plugin.enabled = on;
        plugin.load = if on {
            PluginLoad::Ready
        } else {
            PluginLoad::Off
        };
        Ok(())
    }

    fn remove_plugin(&self, id: &str) -> Result<(), PluginIssue> {
        self.call(format!("remove_plugin {id}"));
        let mut plugins = lock(&self.plugins);
        let before = plugins.len();
        plugins.retain(|p| p.id != id);
        if plugins.len() == before {
            return Err(PluginIssue::NotInstalled { id: id.to_owned() });
        }
        Ok(())
    }

    fn clients(&self) -> Vec<PairedClient> {
        lock(&self.clients).clone()
    }

    fn start_pairing(&self) -> Result<Pairing, PairError> {
        self.call("start_pairing".to_owned());
        match lock(&self.status).lan.clone() {
            Lan::Off => Err(PairError::LanOff),
            Lan::Down { detail } => Err(PairError::LanDown { detail }),
            Lan::Listening { url, fingerprint } => Ok(Pairing {
                code: "123456".to_owned(),
                expires_at: now_ms() + 120_000,
                offer: PairingOffer {
                    url,
                    code: "123456".to_owned(),
                    fingerprint,
                },
            }),
        }
    }

    fn set_client_scopes(&self, id: &str, scopes: Vec<Scope>) -> Result<(), ClientIssue> {
        self.call(format!("set_client_scopes {id} {scopes:?}"));
        if let Some(detail) = lock(&self.clients_write_error).clone() {
            return Err(ClientIssue::Write { detail });
        }
        let mut clients = lock(&self.clients);
        let client = clients
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ClientIssue::UnknownClient { id: id.to_owned() })?;
        client.scopes = scopes;
        Ok(())
    }

    fn remove_client(&self, id: &str) -> Result<(), ClientIssue> {
        self.call(format!("remove_client {id}"));
        if let Some(detail) = lock(&self.clients_write_error).clone() {
            return Err(ClientIssue::Write { detail });
        }
        let mut clients = lock(&self.clients);
        let before = clients.len();
        clients.retain(|c| c.id != id);
        if clients.len() == before {
            return Err(ClientIssue::UnknownClient { id: id.to_owned() });
        }
        Ok(())
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

    fn preview_overlay(&self, view: PreviewView) -> Result<(), PreviewIssue> {
        self.call(format!("preview_overlay {view:?}"));
        match *lock(&self.preview_error) {
            Some(issue) => Err(issue),
            None => Ok(()),
        }
    }

    fn retry_dictation(&self) {
        self.call("retry_dictation".to_owned());
        let mut status = lock(&self.status);
        if matches!(status.dictation, Dictation::Failed { .. }) {
            status.dictation = Dictation::Recording;
        }
    }

    fn update(&self) -> UpdateState {
        lock(&self.update).clone()
    }

    fn check_update(&self) {
        self.call("check_update".to_owned());
    }

    fn upgrade(&self) -> Result<(), UpdateIssue> {
        self.call("upgrade".to_owned());
        let result = lock(&self.upgrade).clone();
        if result.is_ok() {
            let mut state = lock(&self.update);
            if let UpdateState::Available { version, .. } = &*state {
                *state = UpdateState::Restart {
                    version: version.clone(),
                };
            }
        }
        result
    }

    fn dismiss_dictation(&self) {
        self.call("dismiss_dictation".to_owned());
        let mut status = lock(&self.status);
        if matches!(status.dictation, Dictation::Failed { .. }) {
            status.dictation = Dictation::Idle;
        }
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

fn sherpa() -> Manifest {
    Manifest {
        id: "sherpa".to_owned(),
        title: Text::localized([("en", "Local Sherpa"), ("zh-CN", "本地 Sherpa")]),
        blurb: Text::localized([
            ("en", "Streaming recognition on this computer"),
            ("zh-CN", "在本机流式识别，音频不会离开这台电脑"),
        ]),
        docs: None,
        streaming: true,
        fields: vec![field(
            "model",
            Text::localized([("en", "Model"), ("zh-CN", "模型")]),
            Text::localized([
                ("en", "A model folder under ~/.cache/sherpa"),
                ("zh-CN", "~/.cache/sherpa 下的模型目录"),
            ]),
            FieldKind::Text,
            false,
            false,
        )],
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
