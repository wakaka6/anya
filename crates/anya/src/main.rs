#![allow(unreachable_pub)]
#![allow(dead_code, unused_imports)]
#![allow(clippy::collapsible_if, clippy::let_unit_value)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The Anya process: build the adapters around one hub, then run a UI or
//! expose the subscription client/server commands.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anya_audio::System as AudioSystem;
use anya_config::{Config, Mode as ConfigMode, Output as ConfigOutput};
use anya_continuous::{Continuous, Settings as ContinuousSettings};
use anya_core::audio::{AudioSource, Device};
use anya_core::desktop::Desktop;
use anya_core::engine::{Choice, Engine, EngineError, SessionParams};
use anya_core::hub::{Hub, HubInfo};
use anya_dictation::{
    Dictation, Input as DictationInput, Mode as DictationMode, Settings as DictationSettings,
};
use anya_gui::Shell;
use anya_hotkey::{Combo, Edge, Hotkeys};
use anya_i18n::Catalog;
use anya_output::Delivery;
use anya_overlay::Overlay;
use anya_plugin_host::{BunPath, PluginDirs, PluginHost, PluginState, Registry, Store};
use anya_plugin_rt::{Registration, Runtime};
use anya_protocol::{ErrorCode, PairingOffer, Scope, Source};
use anya_runtime::{
    ClientIssue, Control, DebugEvent, Delivered, Dictation as UiDictation, EngineInfo, Entry,
    HotkeyRole, Info, InstalledPlugin, Invalid, Lan, MarketPlugin, Notice, PairError, PairedClient,
    Pairing, PluginIssue, PluginLoad, PreviewIssue, PreviewView, SaveError, Saved, Stats, Status,
    Subscription,
};
use anya_server::{Address, Clients, ClientsError, Gateway, Server, TlsIdentity, parse_address};
use clap::Parser;
use tokio::sync::{Mutex as AsyncMutex, watch};

mod builtin;
mod cli;
mod doctor;
mod indicator;
mod install;
mod plugins;
mod store;
mod timing;

use builtin::Points;
use cli::{Command, Mode, Run};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn choice(config: Arc<Mutex<Config>>, points: Points) -> Choice {
    Arc::new(move || {
        let cfg = lock(&config).clone();
        let id = cfg.recognition.engine_id().to_owned();
        let engine = points
            .engines
            .all()
            .into_iter()
            .find(|engine| engine.manifest().id == id)
            .ok_or_else(|| EngineError::NotConfigured("engine".to_owned()))?;
        let settings = cfg.recognition.settings(&id);
        engine.check(&settings)?;
        let (hotwords, _) = anya_config::read_hotwords(&cfg.recognition);
        Ok((
            engine,
            SessionParams {
                language: cfg.recognition.language(),
                hotwords,
                settings,
            },
        ))
    })
}

/// How long the overlay preview stays before the overlay shows its real state.
const PREVIEW_HOLD: Duration = Duration::from_millis(2500);

/// How often the hotwords are compared with what the last session was given.
const HOTWORDS_POLL: Duration = Duration::from_secs(2);

/// Bumps `params` whenever the merged hotwords (the config's and the file's)
/// change, so a long continuous session reconnects once and takes them.
async fn watch_hotwords(config: Arc<Mutex<Config>>, params: watch::Sender<u64>, poll: Duration) {
    fn read(config: &Mutex<Config>) -> Vec<String> {
        let recognition = lock(config).recognition.clone();
        anya_config::read_hotwords(&recognition).0
    }
    let mut last = read(&config);
    let mut tick = tokio::time::interval(poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let shared = config.clone();
        let Ok(now) = tokio::task::spawn_blocking(move || read(&shared)).await else {
            return;
        };
        if now != last {
            last = now;
            params.send_modify(|value| *value = value.wrapping_add(1));
        }
    }
}

struct App {
    config_path: PathBuf,
    config: Arc<Mutex<Config>>,
    desktop: Desktop,
    points: Points,
    builtin_rt: Mutex<Runtime<Points>>,
    /// The engines of loaded community plugins, served at the engines point.
    plugin_served: Arc<Mutex<Vec<Registration>>>,
    hub: Hub,
    audio: Arc<dyn AudioSource>,
    dictation: Dictation,
    continuous: Continuous,
    hotkeys: Arc<Mutex<Option<Hotkeys>>>,
    status: Arc<Mutex<Status>>,
    activity: store::ActivityStore,
    stats: store::StatsStore,
    quit: watch::Sender<bool>,
    runtime: tokio::runtime::Handle,
    server: AsyncMutex<Option<Server>>,
    /// The recording overlay and the language its labels were made in.
    overlay: Mutex<Option<(Overlay, &'static str)>>,
    /// Bumped when a new overlay window needs the current view again.
    overlay_resend: watch::Sender<u64>,
    choice: Choice,
    clients: Clients,
    state_dir: PathBuf,
    /// The LAN listener's port while sharing is on; changes restart it.
    lan_wanted: watch::Sender<Option<u16>>,
    lan_server: AsyncMutex<Option<Server>>,
    data_dir: PathBuf,
    plugin_store: Store,
    /// Running once a plugin is enabled and a Bun is found.
    plugin_host: Arc<Mutex<Option<PluginHost>>>,
}

impl App {
    async fn start(
        path: PathBuf,
        cfg: Config,
        desktop: Desktop,
        runtime: tokio::runtime::Handle,
    ) -> Result<Arc<App>, String> {
        let config = Arc::new(Mutex::new(cfg.clone()));
        let points = Points::default();
        let mut builtin_rt =
            Runtime::new(points.clone(), builtin::plugins()).map_err(|e| e.to_string())?;
        builtin::reconcile(&mut builtin_rt, &cfg);
        let hub = Hub::new(HubInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            session_prefix: format!("anya-{}", now_ms()),
        });
        let audio: Arc<dyn AudioSource> = Arc::new(AudioSystem::new(cfg.audio.gain));
        let choice = choice(config.clone(), points.clone());
        let (dictation, status_rx, finished_rx) = anya_dictation::spawn(
            hub.clone(),
            audio.clone(),
            choice.clone(),
            DictationSettings {
                mode: match cfg.dictation.mode {
                    ConfigMode::Toggle => DictationMode::Toggle,
                    ConfigMode::Hold => DictationMode::Hold,
                },
                stop_delay: Duration::from_millis(u64::from(cfg.dictation.stop_delay_ms)),
                final_timeout: anya_dictation::FINAL_TIMEOUT,
                device: (!cfg.dictation.device.trim().is_empty())
                    .then(|| cfg.dictation.device.clone()),
            },
        );
        let (params_tx, params_rx) = watch::channel(0_u64);
        let continuous =
            anya_continuous::spawn(hub.clone(), audio.clone(), choice.clone(), params_rx);
        let (quit, _) = watch::channel(false);
        let state_dir = anya_config::state_dir().map_err(|e| e.to_string())?;
        let data_dir = anya_config::data_dir().map_err(|e| e.to_string())?;
        let stats = store::StatsStore::open(state_dir.join("stats.json"));
        let activity = store::ActivityStore::open(store::activity_path(&state_dir, &path));
        let clients = Clients::open(state_dir.join("clients.json"));
        let (lan_wanted, _) = watch::channel(lan_port(&cfg));
        let app = Arc::new(App {
            config_path: path,
            config: config.clone(),
            desktop,
            points,
            builtin_rt: Mutex::new(builtin_rt),
            plugin_served: Arc::default(),
            hub,
            audio,
            dictation,
            continuous,
            hotkeys: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(Status {
                dictation: UiDictation::Idle,
                finishing: 0,
                live_text: None,
                level: None,
                continuous_on: cfg.continuous.enabled,
                subscription: Subscription::Down {
                    detail: "not started".to_owned(),
                },
                lan: Lan::Off,
                stats: stats.get(),
            })),
            activity,
            stats,
            quit,
            runtime,
            server: AsyncMutex::new(None),
            overlay: Mutex::new(None),
            overlay_resend: watch::channel(0).0,
            choice,
            clients,
            state_dir,
            lan_wanted,
            lan_server: AsyncMutex::new(None),
            plugin_store: Store::new(PluginDirs::under(&data_dir)),
            data_dir,
            plugin_host: Arc::new(Mutex::new(None)),
        });
        app.apply_hotkeys()?;
        app.continuous.set(
            cfg.continuous.enabled,
            ContinuousSettings {
                device: (!cfg.continuous.device.trim().is_empty())
                    .then(|| cfg.continuous.device.clone()),
                silence_gate: cfg.continuous.silence_gate,
                final_timeout: Duration::from_secs(2),
            },
        );
        let (done_tx, done_rx) = tokio::sync::mpsc::unbounded_channel();
        app.apply_overlay(&cfg);
        tokio::spawn(app.clone().drive_overlay(status_rx.clone(), done_rx));
        app.spawn_observers(status_rx, finished_rx, done_tx);
        tokio::spawn(watch_hotwords(config.clone(), params_tx, HOTWORDS_POLL));
        let mut edits = anya_config::watch(app.config_path.clone());
        let editor = app.clone();
        tokio::spawn(async move {
            while let Some(read) = edits.recv().await {
                editor.reload(read);
            }
        });
        app.start_subscription(&cfg.subscription.address).await?;
        let plugins_down = |detail: Option<String>| {
            app.activity.push(Entry::Notice {
                at: now_ms(),
                notice: Notice::PluginsUnavailable,
                detail,
            });
        };
        match app.plugin_store.enabled() {
            Ok(enabled) if enabled.is_empty() => {}
            Ok(_) => {
                if let Err(issue) = app.ensure_plugin_host().await {
                    tracing::warn!(%issue, "plugins are not running");
                    plugins_down(None);
                }
            }
            Err(e) => {
                tracing::warn!("plugins are not running: {e}");
                plugins_down(Some(e.to_string()));
            }
        }
        let mut wanted = app.lan_wanted.subscribe();
        app.apply_lan(lan_port(&cfg)).await;
        let follow = app.clone();
        tokio::spawn(async move {
            while wanted.changed().await.is_ok() {
                let port = *wanted.borrow_and_update();
                follow.apply_lan(port).await;
            }
        });
        Ok(app)
    }

    /// Starts the plugin host if it is not running: finds a Bun, and keeps
    /// the engine list in step with what the host loads.
    async fn ensure_plugin_host(&self) -> Result<(), PluginIssue> {
        if lock(&self.plugin_host).is_some() {
            return Ok(());
        }
        let dirs = PluginDirs::under(&self.data_dir);
        let bun = BunPath::detect(&dirs.runtime)
            .await
            .ok_or_else(plugins::bun_missing)?;
        let mut slot = lock(&self.plugin_host);
        if slot.is_some() {
            return Ok(());
        }
        let host = PluginHost::start(dirs, bun);
        let mut changes = host.changes();
        *slot = Some(host);
        drop(slot);
        let (points, host, served) = (
            self.points.clone(),
            self.plugin_host.clone(),
            self.plugin_served.clone(),
        );
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let loaded = lock(&host)
                    .as_ref()
                    .map(PluginHost::engines)
                    .unwrap_or_default();
                let fresh: Vec<Registration> = loaded
                    .into_iter()
                    .map(|engine| points.engines.serve(engine))
                    .collect();
                *lock(&served) = fresh;
            }
        });
        Ok(())
    }

    /// Makes the host load what is enabled now, if a host runs.
    fn reload_plugins(&self) {
        if let Some(host) = lock(&self.plugin_host).as_ref() {
            host.reload();
        }
    }

    /// Stops the plugin host, if any, and waits for its process.
    async fn shutdown_plugins(&self) {
        let host = lock(&self.plugin_host).take();
        if let Some(host) = host {
            host.shutdown().await;
        }
        lock(&self.plugin_served).clear();
    }

    /// What the servers need to take remote dictation: the engine choice
    /// dictation uses, the paired clients and the same final-result wait.
    fn gateway(&self) -> Gateway {
        Gateway {
            choice: self.choice.clone(),
            clients: self.clients.clone(),
            final_timeout: anya_dictation::FINAL_TIMEOUT,
        }
    }

    /// Brings the LAN listener to what the config asks: stopped for `None`,
    /// otherwise listening on that port with the stored certificate. The
    /// outcome, a failure included, goes to the status.
    async fn apply_lan(&self, port: Option<u16>) {
        let mut slot = self.lan_server.lock().await;
        if let Some(old) = slot.take() {
            old.stop().await;
        }
        let lan = match port {
            None => Lan::Off,
            Some(port) => match self.start_lan(port).await {
                Ok((server, fingerprint)) => {
                    let url = server.url();
                    *slot = Some(server);
                    Lan::Listening { url, fingerprint }
                }
                Err(detail) => {
                    tracing::warn!(%detail, "LAN sharing is not listening");
                    Lan::Down { detail }
                }
            },
        };
        lock(&self.status).lan = lan;
    }

    async fn start_lan(&self, port: u16) -> Result<(Server, String), String> {
        let identity =
            TlsIdentity::load_or_create(&self.state_dir.join("tls")).map_err(|e| e.to_string())?;
        let server = Server::start_lan(self.hub.clone(), self.gateway(), port, &identity)
            .await
            .map_err(|e| e.to_string())?;
        Ok((server, identity.fingerprint()))
    }

    fn spawn_observers(
        &self,
        mut status_rx: watch::Receiver<anya_dictation::Status>,
        mut finished: tokio::sync::mpsc::UnboundedReceiver<anya_dictation::Finished>,
        done_tx: tokio::sync::mpsc::UnboundedSender<(String, Delivered)>,
    ) {
        let status = self.status.clone();
        tokio::spawn(async move {
            while status_rx.changed().await.is_ok() {
                let current = status_rx.borrow().clone();
                let mut state = lock(&status);
                state.dictation = match current.state {
                    anya_dictation::State::Idle => UiDictation::Idle,
                    anya_dictation::State::Recording => UiDictation::Recording,
                    anya_dictation::State::Stopping => UiDictation::Stopping,
                    anya_dictation::State::Failed { error, detail } => {
                        UiDictation::Failed { error, detail }
                    }
                };
                state.finishing = current.finishing;
            }
        });
        let status = self.status.clone();
        let config = self.config.clone();
        let activity = self.activity.clone();
        let stats = self.stats.clone();
        let desktop = self.desktop;
        tokio::spawn(async move {
            while let Some(done) = finished.recv().await {
                let cfg = lock(&config).clone();
                let output = match cfg.dictation.output {
                    ConfigOutput::Paste => Some(Delivery::Paste),
                    ConfigOutput::Clipboard => Some(Delivery::Clipboard),
                    ConfigOutput::None => None,
                };
                let delivered = match output {
                    None => Delivered::Kept,
                    Some(how) => match tokio::task::spawn_blocking({
                        let text = done.text.clone();
                        move || anya_output::deliver(desktop, &text, how)
                    })
                    .await
                    {
                        Ok(Ok(())) => match how {
                            Delivery::Paste => Delivered::Pasted,
                            Delivery::Clipboard => Delivered::Copied,
                        },
                        Ok(Err(e)) => {
                            tracing::warn!(error = %e, "text delivery failed");
                            Delivered::Kept
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "text delivery task failed");
                            Delivered::Kept
                        }
                    },
                };
                let _ = done_tx.send((done.text.clone(), delivered));
                stats.record(&done.text, done.audio_ms);
                activity.push(Entry::Final {
                    at: now_ms(),
                    source: Source::Dictation,
                    text: done.text,
                    delivered,
                });
                lock(&status).stats = stats.get();
            }
        });
    }

    async fn start_subscription(&self, raw: &str) -> Result<(), String> {
        let address = parse_address(if raw.trim().is_empty() {
            anya_config::DEFAULT_ADDRESS
        } else {
            raw
        })
        .map_err(|e| e.to_string())?;
        let server = Server::start(self.hub.clone(), address, None, Some(self.gateway()))
            .await
            .map_err(|e| e.to_string())?;
        let url = server.url();
        lock(&self.status).subscription = Subscription::Listening {
            url,
            subscribers: server.subscribers(),
        };
        *self.server.lock().await = Some(server);
        Ok(())
    }

    fn apply_hotkeys(&self) -> Result<(), String> {
        let cfg = lock(&self.config).clone();
        let (hotkeys, mut events) = Hotkeys::start(self.desktop).map_err(|e| e.to_string())?;
        let dictation =
            anya_hotkey::parse_list(&cfg.dictation.hotkeys).map_err(|e| e.to_string())?;
        let continuous =
            anya_hotkey::parse_list(&cfg.continuous.hotkey).map_err(|e| e.to_string())?;
        let mut all = dictation.clone();
        all.extend(continuous.iter().copied());
        // Debug mode also watches these; one that is a hotkey already is
        // registered once.
        let mut watched = all.clone();
        for combo in cfg
            .debug
            .hotkeys
            .iter()
            .filter_map(|h| anya_hotkey::parse_list(h).ok())
            .flatten()
        {
            if !all.contains(&combo) {
                all.push(combo);
            }
            if !watched.contains(&combo) {
                watched.push(combo);
            }
        }
        if !cfg.debug.enabled {
            all.truncate(dictation.len() + continuous.len());
        }
        hotkeys.set(&all).map_err(|e| e.to_string())?;
        let dict = self.dictation.clone();
        let cont = self.continuous.clone();
        let status = self.status.clone();
        let cfg_ref = self.config.clone();
        let activity = self.activity.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if watched.contains(&event.combo) && lock(&cfg_ref).debug.enabled {
                    let combo = event.combo.to_string();
                    activity.push(Entry::Debug {
                        at: now_ms(),
                        event: match event.edge {
                            Edge::Down => DebugEvent::KeyDown { combo },
                            Edge::Up => DebugEvent::KeyUp { combo },
                        },
                    });
                }
                if let Some(index) = dictation.iter().position(|c| *c == event.combo) {
                    let input = match event.edge {
                        Edge::Down => DictationInput::Press(index as u32),
                        Edge::Up => DictationInput::Release(index as u32),
                    };
                    dict.input(input);
                }
                if continuous.contains(&event.combo) && event.edge == Edge::Down {
                    let cfg = lock(&cfg_ref).clone();
                    let on = !lock(&status).continuous_on;
                    cont.set(
                        on,
                        ContinuousSettings {
                            device: (!cfg.continuous.device.trim().is_empty())
                                .then(|| cfg.continuous.device.clone()),
                            silence_gate: cfg.continuous.silence_gate,
                            final_timeout: Duration::from_secs(2),
                        },
                    );
                    lock(&status).continuous_on = on;
                }
            }
        });
        *lock(&self.hotkeys) = Some(hotkeys);
        Ok(())
    }

    /// Starts, restyles or closes the overlay to match `cfg`. A start that
    /// fails is a notice; dictation goes on without the overlay.
    fn apply_overlay(&self, cfg: &Config) {
        let catalog = Catalog::for_setting(&cfg.ui.language);
        let style = indicator::style(&cfg.overlay);
        let mut slot = lock(&self.overlay);
        if !cfg.overlay.enabled {
            *slot = None;
            return;
        }
        if let Some((overlay, locale)) = slot.as_ref() {
            if *locale == catalog.locale() {
                overlay.restyle(style);
                return;
            }
        }
        *slot = None;
        match Overlay::start(self.desktop, style, indicator::labels(&catalog)) {
            Ok(overlay) => {
                *slot = Some((overlay, catalog.locale()));
                self.overlay_resend.send_modify(|n| *n = n.wrapping_add(1));
            }
            Err(e) => {
                tracing::warn!(error = %e, "the overlay did not start");
                self.activity.push(Entry::Notice {
                    at: now_ms(),
                    notice: Notice::OverlayUnavailable,
                    detail: Some(e.to_string()),
                });
            }
        }
    }

    /// Shows what the user's own dictation is doing on the overlay. Other
    /// sessions on the hub, a gateway client's included, never show.
    async fn drive_overlay(
        self: Arc<Self>,
        mut status: watch::Receiver<anya_dictation::Status>,
        mut done: tokio::sync::mpsc::UnboundedReceiver<(String, Delivered)>,
    ) {
        let mut hub = self.hub.subscribe();
        let mut indicator = indicator::Indicator::default();
        for frame in &hub.snapshot {
            indicator.frame(frame);
        }
        indicator.status(status.borrow_and_update().clone());
        let mut timing = timing::Timing::default();
        let mut resend = self.overlay_resend.subscribe();
        let mut words = (String::new(), Catalog::for_setting(""));
        let mut shown: Option<anya_overlay::View> = None;
        loop {
            tokio::select! {
                frame = hub.frames.recv() => match frame {
                    Ok(frame) => {
                        indicator.frame(&frame);
                        if let Some(event) = timing.frame(&frame, Instant::now())
                            && lock(&self.config).debug.enabled
                        {
                            self.activity.push(Entry::Debug { at: now_ms(), event });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                changed = status.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let current = status.borrow_and_update().clone();
                    timing.status(&current, Instant::now());
                    indicator.status(current);
                }
                text = done.recv() => match text {
                    Some((text, delivered)) => indicator.done(text, delivered),
                    None => return,
                },
                _ = resend.changed() => shown = None,
            }
            lock(&self.status).level = indicator.level();
            let setting = lock(&self.config).ui.language.clone();
            if words.0 != setting {
                words = (setting.clone(), Catalog::for_setting(&setting));
            }
            let view = indicator.view(&words.1);
            if shown.as_ref() != Some(&view) {
                if let Some((overlay, _)) = lock(&self.overlay).as_ref() {
                    overlay.show(view.clone());
                }
                shown = Some(view);
            }
        }
    }

    /// Applies a config file that changed on disk, as a save from the
    /// settings would. One that does not parse leaves the config in effect.
    fn reload(&self, read: Result<Config, anya_config::ConfigError>) {
        let current = lock(&self.config).clone();
        let (notice, detail) = match reloaded(read, &current) {
            Reloaded::Same => return,
            Reloaded::Apply(cfg) => {
                if let Some(failed) = self.apply(*cfg) {
                    tracing::warn!(detail = %failed, "hotkeys not registered after reload");
                }
                (Notice::ConfigReloaded, None)
            }
            Reloaded::Broken(detail) => (Notice::ConfigBroken, Some(detail)),
        };
        self.activity.push(Entry::Notice {
            at: now_ms(),
            notice,
            detail,
        });
    }

    /// Makes `cfg` the config in effect: dictation, continuous, hotkeys, the
    /// LAN listener and the overlay follow it. Returns why the hotkeys could
    /// not be registered, if they could not.
    fn apply(&self, cfg: Config) -> Option<String> {
        *lock(&self.config) = cfg.clone();
        builtin::reconcile(&mut lock(&self.builtin_rt), &cfg);
        self.dictation.configure(DictationSettings {
            mode: match cfg.dictation.mode {
                ConfigMode::Toggle => DictationMode::Toggle,
                ConfigMode::Hold => DictationMode::Hold,
            },
            stop_delay: Duration::from_millis(u64::from(cfg.dictation.stop_delay_ms)),
            final_timeout: anya_dictation::FINAL_TIMEOUT,
            device: (!cfg.dictation.device.trim().is_empty()).then(|| cfg.dictation.device.clone()),
        });
        self.continuous.set(
            cfg.continuous.enabled,
            ContinuousSettings {
                device: (!cfg.continuous.device.trim().is_empty())
                    .then(|| cfg.continuous.device.clone()),
                silence_gate: cfg.continuous.silence_gate,
                final_timeout: Duration::from_secs(2),
            },
        );
        lock(&self.status).continuous_on = cfg.continuous.enabled;
        let hotkeys_failed = self.apply_hotkeys().err();
        self.lan_wanted.send_if_modified(|wanted| {
            let next = lan_port(&cfg);
            let changed = *wanted != next;
            *wanted = next;
            changed
        });
        self.apply_overlay(&cfg);
        hotkeys_failed
    }

    fn set_config(&self, cfg: Config) -> Result<Option<String>, SaveError> {
        // Validate against the engines that will be served once `cfg` applies.
        let builtin_ids = builtin::ids();
        let mut ids: Vec<String> = builtin::composed(&cfg, &builtin_ids)
            .into_iter()
            .filter(|c| c.on)
            .map(|c| c.id)
            .collect();
        ids.extend(
            self.points
                .engines
                .all()
                .iter()
                .map(|e| e.manifest().id.clone())
                .filter(|id| !builtin_ids.contains(id)),
        );
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut problems = cfg.problems(&ids);
        if let Err(e) = anya_hotkey::parse_list(&cfg.dictation.hotkeys) {
            problems.push(anya_config::Problem::UnknownPosition(e.to_string()));
        }
        if !problems.is_empty() {
            return Err(SaveError::Invalid(
                problems
                    .into_iter()
                    .map(|problem| match problem {
                        anya_config::Problem::UnknownPosition(value) => {
                            Invalid::HotkeyUnparsable { value }
                        }
                        anya_config::Problem::BadScale => Invalid::BadScale,
                        anya_config::Problem::UnknownEngine(id) => Invalid::UnknownEngine { id },
                    })
                    .collect(),
            ));
        }
        anya_config::save(&cfg, &self.config_path).map_err(|e| SaveError::Write(e.to_string()))?;
        Ok(self.apply(cfg))
    }
}

/// What a change of the config file asks for.
#[derive(Debug, PartialEq)]
enum Reloaded {
    /// The file says what is in effect already: Anya's own save, or a
    /// rewrite of the same values.
    Same,
    /// A new config to apply.
    Apply(Box<Config>),
    /// The file does not parse; the config in effect stays. The parser's
    /// words, untranslated.
    Broken(String),
}

fn reloaded(read: Result<Config, anya_config::ConfigError>, current: &Config) -> Reloaded {
    match read {
        Ok(cfg) if cfg == *current => Reloaded::Same,
        Ok(cfg) => Reloaded::Apply(Box::new(cfg)),
        Err(anya_config::ConfigError::Parse { message, .. }) => Reloaded::Broken(message),
        Err(e) => Reloaded::Broken(e.to_string()),
    }
}

/// `error` as the code the UIs translate.
fn client_issue(error: ClientsError) -> ClientIssue {
    match error {
        ClientsError::UnknownClient(id) => ClientIssue::UnknownClient { id },
        ClientsError::Write(detail) => ClientIssue::Write { detail },
    }
}

/// The LAN listener's port when the config turns sharing on.
fn lan_port(cfg: &Config) -> Option<u16> {
    cfg.subscription.lan.then_some(cfg.subscription.lan_port)
}

impl Control for App {
    fn info(&self) -> Info {
        Info {
            platform: if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            },
            desktop: self.desktop.to_string(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            config_path: self.config_path.clone(),
        }
    }

    fn config(&self) -> Config {
        lock(&self.config).clone()
    }

    fn save(&self, config: Config) -> Result<Saved, SaveError> {
        let hotkeys_failed = self.set_config(config)?;
        Ok(Saved { hotkeys_failed })
    }

    fn check_hotkeys(&self, value: &str, role: HotkeyRole) -> Result<String, Invalid> {
        if value.trim().is_empty() {
            return Ok(String::new());
        }
        let combos = anya_hotkey::parse_list(value).map_err(|_| Invalid::HotkeyUnparsable {
            value: value.to_owned(),
        })?;
        for combo in &combos {
            combo.check_global().map_err(|bad| match bad {
                anya_hotkey::Disallowed::TextKey(c) => Invalid::HotkeyTextKey {
                    combo: c.to_string(),
                },
                anya_hotkey::Disallowed::FnWithFunctionKey(c) => Invalid::HotkeyFnFunctionKey {
                    combo: c.to_string(),
                },
                anya_hotkey::Disallowed::Empty => Invalid::HotkeyUnparsable {
                    value: value.to_owned(),
                },
            })?;
        }
        if role == HotkeyRole::Continuous {
            let dict =
                anya_hotkey::parse_list(&lock(&self.config).dictation.hotkeys).unwrap_or_default();
            if let Some(combo) = combos.iter().find(|combo| dict.contains(combo)) {
                return Err(Invalid::HotkeyTaken {
                    combo: combo.to_string(),
                });
            }
        }
        Ok(anya_hotkey::format_list(&combos))
    }

    fn engines(&self) -> Vec<EngineInfo> {
        self.points
            .engines
            .all()
            .iter()
            .map(|engine| EngineInfo {
                manifest: engine.manifest().clone(),
                ready: engine
                    .check(
                        &lock(&self.config)
                            .recognition
                            .settings(&engine.manifest().id),
                    )
                    .is_ok(),
            })
            .collect()
    }

    fn engine_ready(&self, id: &str, settings: &BTreeMap<String, String>) -> bool {
        self.points
            .engines
            .all()
            .iter()
            .find(|e| e.manifest().id == id)
            .is_some_and(|e| e.check(settings).is_ok())
    }

    fn devices(&self) -> Result<Vec<Device>, String> {
        self.audio.devices().map_err(|e| e.to_string())
    }

    fn status(&self) -> Status {
        let mut status = lock(&self.status).clone();
        status.stats = self.stats.get();
        status
    }

    fn activity(&self) -> Vec<Entry> {
        self.activity.get()
    }

    fn plugins(&self) -> Vec<InstalledPlugin> {
        let host = lock(&self.plugin_host);
        let status = host.as_ref().map(PluginHost::status).unwrap_or_default();
        let running = host.is_some();
        drop(host);
        self.plugin_store
            .installed()
            .unwrap_or_else(|e| {
                tracing::warn!("cannot list installed plugins: {e}");
                Vec::new()
            })
            .into_iter()
            .map(|p| {
                let seen = status.iter().find(|s| s.id == p.id);
                let load = match (p.enabled, seen) {
                    (false, _) => PluginLoad::Off,
                    (true, None) if running => PluginLoad::Loading,
                    (true, None) => PluginLoad::HostDown,
                    (true, Some(s)) => match &s.state {
                        PluginState::Loading => PluginLoad::Loading,
                        PluginState::Ready => PluginLoad::Ready,
                        PluginState::Failed(detail) => PluginLoad::Failed {
                            detail: detail.clone(),
                        },
                    },
                };
                InstalledPlugin {
                    title: seen
                        .and_then(|s| s.manifest.as_ref())
                        .map(|m| m.title.clone()),
                    description: seen
                        .and_then(|s| s.manifest.as_ref())
                        .map(|m| m.blurb.clone()),
                    id: p.id,
                    version: p.version,
                    enabled: p.enabled,
                    load,
                }
            })
            .collect()
    }

    fn plugin_market(&self) -> Result<Vec<MarketPlugin>, PluginIssue> {
        let url = plugins::index_url(&lock(&self.config));
        let index = self
            .runtime
            .block_on(Registry::fetch(&url))
            .map_err(|e| plugins::issue(&e))?;
        Ok(index
            .plugins
            .into_iter()
            .filter_map(|p| {
                let release = p.versions.last()?;
                Some(MarketPlugin {
                    version: release.version.clone(),
                    streaming: release.streaming,
                    permissions: release.permissions.clone(),
                    id: p.id,
                    name: p.name,
                    description: p.description,
                })
            })
            .collect())
    }

    fn install_plugin(&self, id: &str) -> Result<(), PluginIssue> {
        let url = plugins::index_url(&lock(&self.config));
        let dirs = PluginDirs::under(&self.data_dir);
        self.runtime
            .block_on(plugins::install(&dirs, &url, id, None, || {}))
            .map_err(|e| plugins::issue(&e))?;
        // A newer version of an enabled plugin comes in disabled.
        self.reload_plugins();
        Ok(())
    }

    fn set_plugin_enabled(&self, id: &str, on: bool) -> Result<(), PluginIssue> {
        if on {
            self.runtime.block_on(self.ensure_plugin_host())?;
            self.plugin_store.enable(id)
        } else {
            self.plugin_store.disable(id)
        }
        .map_err(|e| plugins::issue(&e))?;
        self.reload_plugins();
        Ok(())
    }

    fn remove_plugin(&self, id: &str) -> Result<(), PluginIssue> {
        self.plugin_store
            .remove(id)
            .map_err(|e| plugins::issue(&e))?;
        self.reload_plugins();
        Ok(())
    }

    fn clients(&self) -> Vec<PairedClient> {
        let mut list: Vec<PairedClient> = self
            .clients
            .list()
            .into_iter()
            .map(|c| PairedClient {
                id: c.id,
                name: c.name,
                scopes: c.scopes,
                paired_at: c.paired_at,
                last_seen: c.last_seen,
                connections: c.connections,
            })
            .collect();
        list.sort_by_key(|c| c.paired_at);
        list
    }

    fn start_pairing(&self) -> Result<Pairing, PairError> {
        let (url, fingerprint) = match lock(&self.status).lan.clone() {
            Lan::Listening { url, fingerprint } => (url, fingerprint),
            Lan::Off => return Err(PairError::LanOff),
            Lan::Down { detail } => return Err(PairError::LanDown { detail }),
        };
        let issued = self.clients.start_pairing();
        Ok(Pairing {
            expires_at: issued.expires_at,
            offer: PairingOffer {
                url,
                code: issued.code.clone(),
                fingerprint,
            },
            code: issued.code,
        })
    }

    fn set_client_scopes(&self, id: &str, scopes: Vec<Scope>) -> Result<(), ClientIssue> {
        self.clients.set_scopes(id, scopes).map_err(client_issue)
    }

    fn remove_client(&self, id: &str) -> Result<(), ClientIssue> {
        self.clients.remove(id).map_err(client_issue)
    }

    fn set_dictation(&self, on: bool) -> Result<Saved, SaveError> {
        let mut cfg = lock(&self.config).clone();
        cfg.dictation.enabled = on;
        self.save(cfg)
    }

    fn set_continuous(&self, on: bool) -> Result<Saved, SaveError> {
        let mut cfg = lock(&self.config).clone();
        cfg.continuous.enabled = on;
        self.save(cfg)
    }

    fn suspend_hotkeys(&self, period: Option<Duration>) {
        if let Some(hotkeys) = lock(&self.hotkeys).as_ref() {
            if period.is_some() {
                let _ = hotkeys.set(&[]);
            } else {
                let _ = self.apply_hotkeys();
            }
        }
    }

    fn record_hotkey(&self, timeout: Duration) -> Result<Option<String>, String> {
        let hotkeys = lock(&self.hotkeys);
        let Some(hotkeys) = hotkeys.as_ref() else {
            return Err("hotkeys are not running".to_owned());
        };
        let combo = self.runtime.block_on(async {
            tokio::time::timeout(timeout, hotkeys.record())
                .await
                .ok()
                .flatten()
        });
        Ok(combo.map(|c| c.to_string()))
    }

    fn copy(&self, text: &str) -> Result<(), String> {
        anya_output::deliver(self.desktop, text, Delivery::Clipboard).map_err(|e| e.to_string())
    }

    fn preview_overlay(&self, view: PreviewView) -> Result<(), PreviewIssue> {
        let catalog = Catalog::for_setting(&lock(&self.config).ui.language);
        let slot = lock(&self.overlay);
        let (overlay, _) = slot.as_ref().ok_or(PreviewIssue::NotRunning)?;
        overlay.show(indicator::sample(view, &catalog));
        drop(slot);
        // Asking for the real view again puts the overlay back as it was.
        let resend = self.overlay_resend.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(PREVIEW_HOLD).await;
            resend.send_modify(|n| *n = n.wrapping_add(1));
        });
        Ok(())
    }

    fn retry_dictation(&self) {
        self.dictation.input(DictationInput::Retry);
    }

    fn dismiss_dictation(&self) {
        self.dictation.input(DictationInput::Cancel);
    }

    fn quit(&self) {
        let _ = self.quit.send(true);
    }
}

struct WebShell {
    control: Arc<App>,
}
impl Shell for WebShell {
    fn kind(&self) -> anya_gui::ShellKind {
        anya_gui::ShellKind::Web
    }
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn pick_file(&self, _start: Option<&Path>) -> Result<Option<PathBuf>, String> {
        Ok(None)
    }
    fn show_settings(&self) {}
    fn quit(&self) {
        self.control.quit();
    }
}

async fn run_app(run: Run, catalog: Catalog) -> Result<(), String> {
    let path = run
        .config
        .unwrap_or(anya_config::default_path().map_err(|e| e.to_string())?);
    let cfg = anya_config::load(&path).map_err(|e| e.to_string())?;
    let desktop = run.backend.unwrap_or_else(Desktop::detect);
    let context = doctor::Context::detect(desktop, &path);
    let engine_list = builtin::engines();
    let chosen_engine = cfg.recognition.engine_id().to_owned();
    let engine = engine_list
        .iter()
        .find(|engine| engine.manifest().id == chosen_engine);
    let mut checks = [
        anya_audio::checks(desktop),
        anya_hotkey::checks(desktop),
        anya_output::checks(desktop),
        anya_overlay::checks(desktop),
    ]
    .concat();
    checks.push(doctor::engine_check(
        &chosen_engine,
        engine.map(|engine| engine.as_ref()),
        &cfg.recognition.settings(&chosen_engine),
        catalog.locale(),
    ));
    let report = doctor::render(&catalog, &context, &checks);
    if run.doctor {
        eprint!("{report}");
        return if anya_core::doctor::healthy(&checks) {
            Ok(())
        } else {
            Err("environment check failed".to_owned())
        };
    }
    if !anya_core::doctor::healthy(&checks) {
        return Err(report);
    }
    // A tray app already running for this config shows its settings instead;
    // starting a second would register every hotkey twice.
    if run.mode == Mode::App
        && anya_gui::desktop::activate_existing(&path).map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let app = App::start(
        path.clone(),
        cfg,
        desktop,
        tokio::runtime::Handle::current(),
    )
    .await?;
    match run.mode {
        Mode::Daemon => {
            let mut quit = app.quit.subscribe();
            let _ = quit.changed().await;
        }
        // The UIs block this thread and may `block_on` the runtime; this keeps
        // them on the main thread, which the desktop event loop requires.
        Mode::Tui => tokio::task::block_in_place(|| anya_tui::run(app.clone(), catalog))
            .map_err(|e| e.to_string())?,
        Mode::Web => {
            let shell = Arc::new(WebShell {
                control: app.clone(),
            });
            let web = anya_gui::web::serve(
                app.clone(),
                shell,
                "127.0.0.1:0"
                    .parse::<std::net::SocketAddr>()
                    .map_err(|e| e.to_string())?,
            )
            .await
            .map_err(|e| e.to_string())?;
            println!("{}", web.url());
            let mut quit = app.quit.subscribe();
            let _ = quit.changed().await;
            web.stop().await;
        }
        Mode::App => {
            let quit = app.quit.subscribe();
            let runtime = tokio::runtime::Handle::current();
            tokio::task::block_in_place(|| {
                anya_gui::desktop::run(app.clone(), catalog, runtime, quit)
            })
            .map_err(|e| e.to_string())?;
        }
    }
    app.shutdown_plugins().await;
    Ok(())
}

async fn run_command(command: Command, catalog: Catalog) -> Result<(), String> {
    match command {
        Command::Run(run) => run_app(run, catalog).await,
        Command::Subscribe {
            config,
            addr,
            timeout,
            max_events,
        } => {
            let path = config.unwrap_or(anya_config::default_path().map_err(|e| e.to_string())?);
            let cfg = anya_config::load(&path).map_err(|e| e.to_string())?;
            let address = addr.unwrap_or_else(|| {
                if cfg.subscription.address.is_empty() {
                    anya_config::DEFAULT_ADDRESS.to_owned()
                } else {
                    cfg.subscription.address
                }
            });
            let parsed = parse_address(&address).map_err(|e| e.to_string())?;
            let exit = anya_server::subscribe(
                parsed,
                max_events,
                timeout,
                tokio::io::stdout(),
                tokio::io::stderr(),
            )
            .await
            .map_err(|e| e.to_string())?;
            if !matches!(
                exit.reason,
                anya_server::ExitReason::Limit
                    | anya_server::ExitReason::Timeout
                    | anya_server::ExitReason::Signal
                    | anya_server::ExitReason::Closed
            ) {
                return Err("subscription ended unexpectedly".to_owned());
            }
            Ok(())
        }
        Command::Plugin { config, action } => {
            let path = config.unwrap_or(anya_config::default_path().map_err(|e| e.to_string())?);
            let cfg = anya_config::load(&path).map_err(|e| e.to_string())?;
            let data = anya_config::data_dir().map_err(|e| e.to_string())?;
            plugins::run(action, &cfg, &data, &catalog, &mut std::io::stdout()).await
        }
        Command::Replay { file, addr, fast } => {
            let text = tokio::fs::read_to_string(&file)
                .await
                .map_err(|e| e.to_string())?;
            let lines = text.lines().map(ToOwned::to_owned).collect();
            let address = parse_address(addr.as_deref().unwrap_or(anya_config::DEFAULT_ADDRESS))
                .map_err(|e| e.to_string())?;
            let server = anya_server::start_replay(lines, address, !fast)
                .await
                .map_err(|e| e.to_string())?;
            tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
            server.stop().await;
            Ok(())
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let catalog = Catalog::for_setting("system");
    let args: Vec<OsString> = std::env::args_os().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == anya_overlay::HELPER_ARG)
    {
        #[cfg(target_os = "macos")]
        anya_overlay::helper_main();
        #[cfg(not(target_os = "macos"))]
        std::process::exit(1);
    }
    let command = match cli::parse(args, &catalog) {
        Ok(command) => command,
        Err(error) => {
            let _ = error.print();
            return;
        }
    };
    if let Command::Run(run) = &command {
        if run.install {
            match install::install() {
                Ok(install::Installed::Copied(path)) => println!(
                    "{}",
                    catalog.tf("cli.install.done", &[("path", &path.display().to_string())])
                ),
                Ok(install::Installed::AlreadyThere(path)) => println!(
                    "{}",
                    catalog.tf(
                        "cli.install.already",
                        &[("path", &path.display().to_string())]
                    )
                ),
                Err(e) => {
                    eprintln!(
                        "{}",
                        catalog.tf("cli.install.failed", &[("detail", &e.to_string())])
                    );
                    std::process::exit(1);
                }
            }
            return;
        }
    }
    if let Err(error) = run_command(command, catalog).await {
        eprintln!("anya: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_applies_a_broken_file_keeps_the_old_config_and_our_own_save_is_ignored() {
        let current = Config::default();
        let mut edited = current.clone();
        edited.overlay.scale = 1.5;
        assert_eq!(
            reloaded(Ok(edited.clone()), &current),
            Reloaded::Apply(Box::new(edited))
        );
        assert_eq!(reloaded(Ok(current.clone()), &current), Reloaded::Same);
        let broken = anya_config::ConfigError::Parse {
            path: PathBuf::from("config.toml"),
            message: "line 2: expected a string".to_owned(),
        };
        assert_eq!(
            reloaded(Err(broken), &current),
            Reloaded::Broken("line 2: expected a string".to_owned())
        );
    }

    #[tokio::test]
    async fn a_changed_hotwords_file_bumps_the_params_once() {
        let file = std::env::temp_dir().join(format!("anya-hotwords-{}.txt", std::process::id()));
        std::fs::write(&file, "Wayland\n").expect("write");
        let mut cfg = Config::default();
        cfg.recognition.hotwords_file = file.to_string_lossy().into_owned();
        let config = Arc::new(Mutex::new(cfg));
        let (tx, mut rx) = watch::channel(0_u64);
        let task = tokio::spawn(watch_hotwords(config, tx, Duration::from_millis(10)));

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!rx.has_changed().expect("open"), "nothing changed yet");
        std::fs::write(&file, "Wayland\nSway\n").expect("write");
        tokio::time::timeout(Duration::from_secs(2), rx.changed())
            .await
            .expect("the edit is noticed")
            .expect("open");
        assert_eq!(*rx.borrow_and_update(), 1);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!rx.has_changed().expect("open"), "one edit, one bump");
        task.abort();
        let _ = std::fs::remove_file(&file);
    }
}
