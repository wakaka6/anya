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
use anya_config::{Config, EngineRole, Mode as ConfigMode, Output as ConfigOutput};
use anya_continuous::{Continuous, Settings as ContinuousSettings};
use anya_core::audio::{AudioSource, Device};
use anya_core::desktop::Desktop;
use anya_core::engine::{Choice, Engine, EngineError, SessionParams};
use anya_core::hooks::{Actions, Call, ContextEngine, HookFailed};
use anya_core::hub::{Hub, HubInfo};
use anya_dictation::{
    Dictation, Input as DictationInput, Mode as DictationMode, Settings as DictationSettings,
};
use anya_gui::Shell;
use anya_hotkey::{Combo, Edge, Hotkeys};
use anya_i18n::Catalog;
use anya_output::Delivery;
use anya_overlay::Overlay;
use anya_plugin_host::{
    BunPath, PluginDirs, PluginHost, PluginState, Registry, Store, read_package,
};
use anya_plugin_rt::{Registration, Runtime};
use anya_protocol::Provide;
use anya_protocol::{ErrorCode, PairingOffer, Scope, Source};
use anya_runtime::{
    BuiltinHooks, ClientIssue, Control, DebugEvent, Delivered, Dictation as UiDictation,
    EngineInfo, Entry, HookInfo, HotkeyRole, Info, InstalledPlugin, Invalid, Lan, Market,
    MarketPlugin, Notice, PairError, PairedClient, Pairing, PluginIssue, PluginLoad, PreviewIssue,
    PreviewView, Route, SaveError, Saved, Stats, Status, Subscription, UpdateIssue, UpdateState,
    UpdateVia,
};
use anya_server::{
    Address, Audience, Clients, ClientsError, Gateway, Server, TlsIdentity, parse_address,
};
use clap::Parser;
use tokio::sync::{Mutex as AsyncMutex, watch};

mod builtin;
mod cli;
mod community;
mod doctor;
mod hooks;
mod hotwords;
mod indicator;
mod install;
mod official;
mod plugins;
mod route;
mod store;
mod timing;

use builtin::Points;
use cli::{Command, Mode, Run};
use community::{Community, Hooks, InlineState};

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

/// The engine `role` uses, read from the config each time a session opens so
/// a saved change applies to the next session of that use; the hotwords
/// file at `hotwords` is read then too. With `context`, the engine asks the
/// context hooks served now before each session opens, about the app that is
/// in front then (docs/decisions/0016).
fn choice(
    config: Arc<Mutex<Config>>,
    points: Points,
    hotwords: PathBuf,
    role: EngineRole,
    context: Option<(Arc<Community>, Desktop)>,
) -> Choice {
    Arc::new(move || {
        let cfg = lock(&config).clone();
        let id = cfg.recognition.engine_id(role).to_owned();
        let engine = points
            .engines
            .all()
            .into_iter()
            .find(|engine| engine.manifest().id == id)
            .ok_or_else(|| EngineError::NotConfigured("engine".to_owned()))?;
        let settings = cfg.recognition.settings(&id);
        engine.check(&settings)?;
        let (hotwords, _) = anya_config::read_hotwords(&cfg.recognition, &hotwords);
        let engine = match &context {
            Some((community, desktop)) => {
                with_context(engine, &cfg, &points, &community.declared(), *desktop)
            }
            None => engine,
        };
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

/// A failed hook as the activity list keeps it: the plugin, the hook and the
/// failure's code, never the text or the plugin's own words.
fn hook_failed(failed: HookFailed, session: Option<anya_protocol::SessionId>) -> Entry {
    log_hook_failed(&failed);
    Entry::HookFailed {
        at: now_ms(),
        plugin: failed.plugin,
        hook: failed.hook,
        failure: failed.failure,
        session,
    }
}

/// Logs a failed hook: its plugin, hook and code, and at debug level the
/// plugin's own words, which may quote what was dictated.
fn log_hook_failed(f: &HookFailed) {
    tracing::warn!(plugin = %f.plugin, hook = ?f.hook, code = ?f.failure, "hook failed");
    tracing::debug!(plugin = %f.plugin, detail = %f.detail, "hook failure detail");
}

/// `engine`, asking the context hooks `points` serve before each session
/// opens; `engine` itself when there are none. A hook that fails is logged.
fn with_context(
    engine: Arc<dyn Engine>,
    cfg: &Config,
    points: &Points,
    declared: &BTreeMap<String, community::Declared>,
    desktop: Desktop,
) -> Arc<dyn Engine> {
    let hooks = hooks::plan(cfg, points, declared)
        .runnable(declared)
        .contexts;
    if hooks.is_empty() {
        return engine;
    }
    let call = Call {
        session: None,
        source: Source::Dictation,
        app: anya_focus::focused_app(desktop),
        settings: BTreeMap::new(),
    };
    let failed = |failures: Vec<HookFailed>| {
        failures.iter().for_each(log_hook_failed);
    };
    Arc::new(ContextEngine::new(engine, hooks, call, Arc::new(failed)))
}

/// How often Anya asks the tap whether a newer version exists.
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Looks for a newer version at start and then daily while `[updates] check`
/// is on; `now` wakes it for a check the user asked for, which ignores that
/// setting. An upgrade in progress or finished is never overwritten.
async fn watch_updates(
    config: Arc<Mutex<Config>>,
    state: Arc<Mutex<UpdateState>>,
    now: Arc<tokio::sync::Notify>,
) {
    let mut asked = false;
    loop {
        let (check, formula) = {
            let cfg = lock(&config);
            (
                cfg.updates.check,
                cfg.updates
                    .formula()
                    .unwrap_or(anya_update::DEFAULT_FORMULA)
                    .to_owned(),
            )
        };
        if check || asked {
            let found = anya_update::latest(&formula).await;
            let mut state = lock(&state);
            if !matches!(
                *state,
                UpdateState::Upgrading { .. } | UpdateState::Restart { .. }
            ) {
                *state = match found {
                    Ok(latest) if anya_update::is_newer(env!("CARGO_PKG_VERSION"), &latest) => {
                        UpdateState::Available {
                            version: latest.to_string(),
                            via: match anya_update::Install::current() {
                                anya_update::Install::Homebrew => UpdateVia::Homebrew,
                                anya_update::Install::Other => UpdateVia::Other,
                            },
                        }
                    }
                    Ok(_) | Err(anya_update::UpdateError::NotPublished { .. }) => {
                        UpdateState::Current
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "update check failed");
                        UpdateState::Failed {
                            detail: e.to_string(),
                        }
                    }
                };
            }
        }
        asked = tokio::select! {
            () = tokio::time::sleep(UPDATE_EVERY) => false,
            () = now.notified() => true,
        };
    }
}

/// Where [`App::audiences`] keeps each server's audience.
const SUBSCRIPTION: usize = 0;
const LAN: usize = 1;

/// The running servers' audiences, read once for one route.
struct Servers {
    audiences: Vec<Audience>,
    /// Paired apps' names by id.
    names: BTreeMap<String, String>,
}

impl route::Reach for Servers {
    fn client_of(&self, session: &anya_protocol::SessionId) -> Option<String> {
        let id = self.audiences.iter().find_map(|a| a.client_of(session))?;
        Some(self.names.get(&id).cloned().unwrap_or(id))
    }

    fn readers_of(&self, session: &anya_protocol::SessionId) -> usize {
        self.audiences.iter().map(|a| a.readers_of(session)).sum()
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
    /// The hooks of community plugins and the inline modules behind some.
    community: Arc<Community>,
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
    remote_choice: Choice,
    clients: Clients,
    state_dir: PathBuf,
    /// The LAN listener's port while sharing is on; changes restart it.
    lan_wanted: watch::Sender<Option<u16>>,
    lan_server: AsyncMutex<Option<Server>>,
    /// Who the subscription server's and the LAN listener's frames reach;
    /// the last one stays readable after its server stops.
    audiences: Arc<Mutex<[Option<Audience>; 2]>>,
    /// The route page's view of the hub.
    router: Arc<Mutex<route::Router>>,
    data_dir: PathBuf,
    plugin_store: Store,
    /// Running once a plugin is enabled and a Bun is found.
    plugin_host: Arc<Mutex<Option<PluginHost>>>,
    /// What the update check knows (docs/decisions/0014).
    update: Arc<Mutex<UpdateState>>,
    /// Wakes the update check for a check the user asked for.
    update_now: Arc<tokio::sync::Notify>,
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
        let community = Community::new();
        let mut builtin_rt = Runtime::new(
            points.clone(),
            builtin::plugins(anya_config::replacements_path(&path), community.clone()),
        )
        .map_err(|e| e.to_string())?;
        builtin::reconcile(&mut builtin_rt, &cfg);
        let hub = Hub::new(HubInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            session_prefix: format!("anya-{}", now_ms()),
        });
        let audio: Arc<dyn AudioSource> = Arc::new(AudioSystem::new(cfg.audio.gain));
        let words_file = anya_config::hotwords_path(&path);
        let choice_for = |role, context| {
            choice(
                config.clone(),
                points.clone(),
                words_file.clone(),
                role,
                context,
            )
        };
        let remote_choice = choice_for(EngineRole::Remote, None);
        let (dictation, status_rx, finished_rx) = anya_dictation::spawn(
            hub.clone(),
            audio.clone(),
            choice_for(EngineRole::Dictation, Some((community.clone(), desktop))),
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
        let continuous = anya_continuous::spawn(
            hub.clone(),
            audio.clone(),
            choice_for(EngineRole::Continuous, None),
            params_rx,
        );
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
            community,
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
            remote_choice,
            clients,
            state_dir,
            lan_wanted,
            lan_server: AsyncMutex::new(None),
            audiences: Arc::default(),
            router: Arc::default(),
            plugin_store: Store::new(PluginDirs::under(&data_dir)),
            data_dir,
            plugin_host: Arc::new(Mutex::new(None)),
            update: Arc::new(Mutex::new(UpdateState::Unknown)),
            update_now: Arc::new(tokio::sync::Notify::new()),
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
        tokio::spawn(app.clone().drive_router());
        app.spawn_observers(status_rx, finished_rx, done_tx);
        let notes = app.clone();
        tokio::spawn(hotwords::watch(
            config.clone(),
            words_file,
            params_tx,
            move |path, error| {
                notes.activity.push(Entry::Notice {
                    at: now_ms(),
                    notice: Notice::HotwordsUnreadable,
                    detail: Some(format!("{}: {error}", path.display())),
                });
            },
            hotwords::POLL,
            hotwords::GAP,
        ));
        let mut edits = anya_config::watch(app.config_path.clone());
        let editor = app.clone();
        tokio::spawn(async move {
            while let Some(read) = edits.recv().await {
                editor.reload(read);
            }
        });
        app.start_subscription(&cfg.subscription.address).await?;
        tokio::spawn(watch_updates(
            app.config.clone(),
            app.update.clone(),
            app.update_now.clone(),
        ));
        let plugins_down = |detail: Option<String>| {
            app.activity.push(Entry::Notice {
                at: now_ms(),
                notice: Notice::PluginsUnavailable,
                detail,
            });
        };
        let updated = plugins::update_official(&app.plugin_store);
        if !updated.is_empty() {
            app.activity.push(Entry::Notice {
                at: now_ms(),
                notice: Notice::PluginsUpdated,
                detail: Some(updated.join(", ")),
            });
        }
        match app.plugin_store.needs_bun() {
            Ok(false) => {}
            Ok(true) => {
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
        app.community.refresh(&app.plugin_store, &app.config).await;
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
    /// the engines and hooks served in step with what the host loads, and
    /// the activity list told of plugins the host had to kill.
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
        let mut stalls = host.stalls();
        *slot = Some(host);
        drop(slot);
        let (points, host, served, community) = (
            self.points.clone(),
            self.plugin_host.clone(),
            self.plugin_served.clone(),
            self.community.clone(),
        );
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let (loaded, hooks) = match lock(&host).as_ref() {
                    Some(host) => (
                        host.engines(),
                        Hooks {
                            contexts: host.contexts(),
                            transforms: host.transforms(),
                            actions: host.actions(),
                        },
                    ),
                    None => Default::default(),
                };
                let fresh: Vec<Registration> = loaded
                    .into_iter()
                    .map(|engine| points.engines.serve(engine))
                    .collect();
                *lock(&served) = fresh;
                community.set_host(hooks);
            }
        });
        let activity = self.activity.clone();
        tokio::spawn(async move {
            loop {
                match stalls.recv().await {
                    Ok(plugin) => activity.push(Entry::PluginStalled {
                        at: now_ms(),
                        plugin,
                    }),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(())
    }

    /// Makes the host load what is enabled now, if a host runs, and the
    /// inline plugins follow.
    fn reload_plugins(&self) {
        if let Some(host) = lock(&self.plugin_host).as_ref() {
            host.reload();
        }
        self.refresh_inline();
    }

    /// Loads the inline plugins of what is enabled now, off the caller's
    /// thread.
    fn refresh_inline(&self) {
        let (community, store, config) = (
            self.community.clone(),
            self.plugin_store.clone(),
            self.config.clone(),
        );
        self.runtime.spawn(async move {
            community.refresh(&store, &config).await;
        });
    }

    /// Whether installed plugin `id` has a host entry, so enabling it needs a
    /// running Bun. A package that cannot be read counts as one that does:
    /// the host says why it did not load.
    fn runs_in_bun(&self, id: &str) -> bool {
        let Ok(installed) = self.plugin_store.installed() else {
            return true;
        };
        installed.iter().find(|p| p.id == id).is_none_or(|p| {
            match read_package(&self.plugin_store.package_dir(p)) {
                Ok(package) => package.anya.host.is_some(),
                Err(_) => true,
            }
        })
    }

    /// Stops the plugin host, if any, and waits for its process.
    async fn shutdown_plugins(&self) {
        let host = lock(&self.plugin_host).take();
        if let Some(host) = host {
            host.shutdown().await;
        }
        lock(&self.plugin_served).clear();
        self.community.set_host(Hooks::default());
    }

    /// What the servers need to take remote dictation: the engine choice
    /// remote dictation uses, the paired clients and the same final-result wait.
    fn gateway(&self) -> Gateway {
        Gateway {
            choice: self.remote_choice.clone(),
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
                    lock(&self.audiences)[LAN] = Some(server.audience());
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
        let failures = self.activity.clone();
        tokio::spawn(async move {
            // A failure is kept once, when the dictation enters it.
            let mut failed = false;
            while status_rx.changed().await.is_ok() {
                let current = status_rx.borrow().clone();
                match &current.state {
                    anya_dictation::State::Failed { error, detail } if !failed => {
                        failures.push(Entry::Failed {
                            at: now_ms(),
                            source: Source::Dictation,
                            error: *error,
                            detail: detail.clone(),
                        });
                    }
                    _ => {}
                }
                failed = matches!(current.state, anya_dictation::State::Failed { .. });
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
        let router = self.router.clone();
        let desktop = self.desktop;
        let points = self.points.clone();
        let community = self.community.clone();
        let (actions, mut action_failures) = Actions::new();
        let failures = activity.clone();
        tokio::spawn(async move {
            while let Some(failed) = action_failures.recv().await {
                failures.push(hook_failed(failed, None));
            }
        });
        tokio::spawn(async move {
            while let Some(done) = finished.recv().await {
                let cfg = lock(&config).clone();
                let declared = community.declared();
                let plan = hooks::plan(&cfg, &points, &declared).runnable(&declared);
                let mut text = done.text;
                if !plan.transforms.is_empty() || !plan.actions.is_empty() {
                    let app = tokio::task::spawn_blocking(move || anya_focus::focused_app(desktop))
                        .await
                        .ok()
                        .flatten();
                    let call = Call {
                        session: Some(done.session.clone()),
                        source: Source::Dictation,
                        app,
                        settings: BTreeMap::new(),
                    };
                    let (chained, failed) =
                        anya_core::hooks::transform(&plan.transforms, text, &call, plan.chain)
                            .await;
                    text = chained;
                    for f in failed {
                        activity.push(hook_failed(f, Some(done.session.clone())));
                    }
                    actions.dispatch(&plan.actions, &text, &call);
                }
                let output = match cfg.dictation.output {
                    ConfigOutput::Paste => Some(Delivery::Paste),
                    ConfigOutput::Clipboard => Some(Delivery::Clipboard),
                    ConfigOutput::None => None,
                };
                let delivered = match output {
                    None => Delivered::Kept,
                    Some(how) => match tokio::task::spawn_blocking({
                        let text = text.clone();
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
                let _ = done_tx.send((text.clone(), delivered));
                stats.record(&text, done.audio_ms);
                let engine = {
                    let mut router = lock(&router);
                    router.delivered(&done.session, delivered);
                    router.engine_of(&done.session)
                };
                activity.push(Entry::Final {
                    at: now_ms(),
                    source: Source::Dictation,
                    text,
                    delivered,
                    session: Some(done.session),
                    engine,
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
        lock(&self.audiences)[SUBSCRIPTION] = Some(server.audience());
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
            {
                let mut status = lock(&self.status);
                status.level = indicator.level();
                status.live_text = indicator.live_text();
            }
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

    /// Follows every session on the hub for the route page.
    async fn drive_router(self: Arc<Self>) {
        let mut hub = self.hub.subscribe();
        {
            let mut router = lock(&self.router);
            for frame in &hub.snapshot {
                router.frame(frame, now_ms());
            }
        }
        loop {
            match hub.frames.recv().await {
                Ok(frame) => lock(&self.router).frame(&frame, now_ms()),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    /// What the servers know, for the route page, with each paired app by
    /// its name.
    fn reach(&self) -> Servers {
        let names = self
            .clients
            .list()
            .into_iter()
            .map(|c| (c.id, c.name))
            .collect();
        Servers {
            audiences: lock(&self.audiences).iter().flatten().cloned().collect(),
            names,
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

    /// Every built-in engine, switched on or off, then the engines community
    /// plugins serve (which have no switch here).
    fn listed_engines(&self) -> Vec<(Arc<dyn Engine>, Option<bool>)> {
        let served = self.points.engines.all();
        let builtin = builtin::engines();
        let mut out: Vec<_> = builtin
            .iter()
            .map(
                |b| match served.iter().find(|s| s.manifest().id == b.manifest().id) {
                    Some(on) => (on.clone(), Some(true)),
                    None => (b.clone(), Some(false)),
                },
            )
            .collect();
        out.extend(
            served
                .into_iter()
                .filter(|s| !builtin.iter().any(|b| b.manifest().id == s.manifest().id))
                .map(|s| (s, None)),
        );
        out
    }

    /// Makes `cfg` the config in effect: dictation, continuous, hotkeys, the
    /// LAN listener and the overlay follow it. Returns why the hotkeys could
    /// not be registered, if they could not.
    fn apply(&self, cfg: Config) -> Option<String> {
        let budgets_changed = std::mem::replace(&mut *lock(&self.config), cfg.clone())
            .plugins
            .limits
            != cfg.plugins.limits;
        builtin::reconcile(&mut lock(&self.builtin_rt), &cfg);
        if budgets_changed {
            // Inline plugins load with the budgets in effect.
            self.refresh_inline();
        }
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
            .filter(|c| c.on && builtin::engines().iter().any(|e| e.manifest().id == c.id))
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
                        anya_config::Problem::UnknownEngine(role, id) => {
                            Invalid::UnknownEngine { role, id }
                        }
                        anya_config::Problem::BadPluginId(_, id) => Invalid::BadPluginId { id },
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
            hotwords_path: anya_config::hotwords_path(&self.config_path),
            replacements_path: anya_config::replacements_path(&self.config_path),
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

    fn hooks(&self) -> Vec<HookInfo> {
        let cfg = lock(&self.config).clone();
        hooks::plan(&cfg, &self.points, &self.community.declared()).infos()
    }

    fn builtin_hooks(&self) -> Vec<BuiltinHooks> {
        let cfg = lock(&self.config).clone();
        let wanted = builtin::composed(&cfg, &builtin::ids());
        let on = |id: &str| wanted.iter().any(|c| c.id == id && c.on);
        vec![
            BuiltinHooks {
                id: builtin::REPLACE.to_owned(),
                provides: vec![Provide::Transform],
                on: on(builtin::REPLACE),
            },
            BuiltinHooks {
                id: builtin::COMMUNITY_HOOKS.to_owned(),
                provides: vec![Provide::Context, Provide::Transform, Provide::Action],
                on: on(builtin::COMMUNITY_HOOKS),
            },
        ]
    }

    fn engines(&self) -> Vec<EngineInfo> {
        self.listed_engines()
            .iter()
            .map(|(engine, enabled)| EngineInfo {
                enabled: *enabled,
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
        self.listed_engines()
            .iter()
            .map(|(engine, _)| engine)
            .find(|e| e.manifest().id == id)
            .is_some_and(|e| e.check(settings).is_ok())
    }

    fn devices(&self) -> Result<Vec<Device>, String> {
        self.audio.devices().map_err(|e| e.to_string())
    }

    fn status(&self) -> Status {
        let mut status = lock(&self.status).clone();
        status.stats = self.stats.get();
        if let Subscription::Listening { subscribers, .. } = &mut status.subscription
            && let Some(audience) = &lock(&self.audiences)[SUBSCRIPTION]
        {
            *subscribers = audience.subscribers();
        }
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
                let package = read_package(&self.plugin_store.package_dir(&p)).ok();
                let host = package.as_ref().is_some_and(|k| k.anya.host.is_some());
                let inline = self.community.inline_state(&p.id);
                let host_load = match seen {
                    None if running => PluginLoad::Loading,
                    None => PluginLoad::HostDown,
                    Some(s) => match &s.state {
                        PluginState::Loading => PluginLoad::Loading,
                        PluginState::Ready => PluginLoad::Ready,
                        PluginState::Failed(detail) => PluginLoad::Failed {
                            detail: detail.clone(),
                        },
                    },
                };
                let load = match (p.enabled, &inline) {
                    (false, _) => PluginLoad::Off,
                    (true, Some(InlineState::Failed(detail))) => PluginLoad::Failed {
                        detail: detail.clone(),
                    },
                    // An inline-only package waits for its module, not for Bun.
                    (true, None) if !host && package.is_some() => PluginLoad::Loading,
                    (true, Some(_)) if !host => PluginLoad::Ready,
                    (true, _) => host_load,
                };
                // What the index said at install, so the enable prompt can
                // say what the plugin will receive before it has ever run;
                // what its entries export once loaded adds to it.
                let mut provides = p.provides.clone();
                let loaded = seen.map(|s| s.provides.clone()).unwrap_or_default();
                let inline_provides = match inline {
                    Some(InlineState::Ready(inline)) => inline,
                    _ => Vec::new(),
                };
                for provide in loaded.into_iter().chain(inline_provides) {
                    if !provides.contains(&provide) {
                        provides.push(provide);
                    }
                }
                let fields = package
                    .as_ref()
                    .map(|k| k.anya.fields.clone())
                    .unwrap_or_default();
                let required: Vec<String> = fields
                    .iter()
                    .filter(|f| f.required)
                    .map(|f| f.key.clone())
                    .collect();
                let missing = lock(&self.config).plugins.unset(&p.id, &required);
                InstalledPlugin {
                    title: seen
                        .and_then(|s| s.manifest.as_ref())
                        .map(|m| m.title.clone())
                        .or_else(|| package.as_ref().map(|k| k.name.clone())),
                    description: seen
                        .and_then(|s| s.manifest.as_ref())
                        .map(|m| m.blurb.clone())
                        .or_else(|| package.as_ref().map(|k| k.description.clone())),
                    id: p.id,
                    version: p.version,
                    enabled: p.enabled,
                    load,
                    provides,
                    host,
                    permissions: package
                        .as_ref()
                        .map(|k| k.anya.permissions.clone())
                        .unwrap_or_default(),
                    fields,
                    missing,
                }
            })
            .collect()
    }

    fn plugin_market(&self) -> Market {
        let url = plugins::index_url(&lock(&self.config));
        let (listed, unreachable) = self.runtime.block_on(plugins::market(&url));
        let plugins = listed
            .into_iter()
            .filter_map(
                |plugins::Listed {
                     plugin: p,
                     official,
                 }| {
                    let release = p.versions.last()?;
                    Some(MarketPlugin {
                        version: release.version.clone(),
                        official,
                        streaming: release.streaming,
                        provides: release.provides.clone(),
                        host: release.host,
                        permissions: release.permissions.clone(),
                        id: p.id,
                        name: p.name,
                        description: p.description,
                    })
                },
            )
            .collect();
        Market {
            plugins,
            unreachable: unreachable.map(|e| plugins::issue(&e)),
        }
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
            // Only a package with a host entry needs Bun.
            if self.runs_in_bun(id) {
                self.runtime.block_on(self.ensure_plugin_host())?;
            }
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

    fn route(&self) -> Route {
        let outputs = route::Outputs {
            delivery: match lock(&self.config).dictation.output {
                ConfigOutput::Paste => Some(Delivered::Pasted),
                ConfigOutput::Clipboard => Some(Delivered::Copied),
                ConfigOutput::None => None,
            },
            overlay: lock(&self.overlay).is_some(),
        };
        let reach = self.reach();
        lock(&self.router).route(&reach, outputs)
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

    fn update(&self) -> UpdateState {
        lock(&self.update).clone()
    }

    fn check_update(&self) {
        self.update_now.notify_one();
    }

    fn upgrade(&self) -> Result<(), UpdateIssue> {
        let (version, via) = match &*lock(&self.update) {
            UpdateState::Available { version, via } => (version.clone(), *via),
            _ => return Err(UpdateIssue::NothingToInstall),
        };
        if via != UpdateVia::Homebrew {
            return Err(UpdateIssue::NotHomebrew);
        }
        *lock(&self.update) = UpdateState::Upgrading {
            version: version.clone(),
        };
        match self.runtime.block_on(anya_update::upgrade()) {
            Ok(()) => {
                *lock(&self.update) = UpdateState::Restart { version };
                Ok(())
            }
            Err(e) => {
                *lock(&self.update) = UpdateState::Available { version, via };
                tracing::warn!(error = %e, "brew upgrade failed");
                Err(match e {
                    anya_update::UpdateError::NotHomebrew => UpdateIssue::NotHomebrew,
                    anya_update::UpdateError::BrewMissing => UpdateIssue::BrewMissing,
                    other => UpdateIssue::Failed {
                        detail: other.to_string(),
                    },
                })
            }
        }
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
    let mut checks = [
        anya_audio::checks(desktop),
        anya_hotkey::checks(desktop),
        anya_output::checks(desktop),
        anya_overlay::checks(desktop),
    ]
    .concat();
    checks.extend(doctor::engine_checks(
        &cfg.recognition,
        &engine_list,
        &catalog,
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
    // Only now: a start that handed over to a running app must not rewrite
    // that app's log.
    init_logs(run.verbose, run.mode == Mode::Daemon);
    let app = App::start(
        path.clone(),
        cfg,
        desktop,
        tokio::runtime::Handle::current(),
    )
    .await?;
    // SIGINT and SIGTERM end the app the way Quit does, so the plugin host and
    // the servers are stopped instead of left to the system.
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (mut interrupt, mut terminate) = (
            signal(SignalKind::interrupt()).map_err(|e| e.to_string())?,
            signal(SignalKind::terminate()).map_err(|e| e.to_string())?,
        );
        let quitting = app.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            quitting.quit();
        });
    }
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
            plugins::run(action, &cfg, &path, &data, &catalog, &mut std::io::stdout()).await
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

/// Sends the log where the mode wants it: stderr with `--daemon`, else
/// `anya.log` in the state directory, rewritten at each start. Info level,
/// debug for Anya's crates with `--verbose`; `RUST_LOG` overrides both.
fn init_logs(verbose: bool, daemon: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(if verbose { "info,anya=debug" } else { "info" }));
    let logs = tracing_subscriber::fmt().with_env_filter(filter);
    if daemon {
        let _ = logs.with_writer(std::io::stderr).try_init();
        return;
    }
    let file = anya_config::state_dir().ok().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::File::create(dir.join("anya.log")).ok()
    });
    if let Some(file) = file {
        let _ = logs
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .try_init();
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

    struct Stub(anya_protocol::Manifest);

    impl anya_core::engine::Engine for Stub {
        fn manifest(&self) -> &anya_protocol::Manifest {
            &self.0
        }
        fn open(&self, _params: anya_core::engine::SessionParams) -> anya_core::engine::Session {
            let (input, _) = tokio::sync::mpsc::channel(1);
            let (_, output) = tokio::sync::mpsc::channel(1);
            anya_core::engine::Session { input, output }
        }
    }

    fn stub(id: &str) -> Arc<dyn anya_core::engine::Engine> {
        Arc::new(Stub(anya_protocol::Manifest {
            id: id.to_owned(),
            title: anya_protocol::Text::localized([("en", id)]),
            blurb: anya_protocol::Text::localized([("en", id)]),
            docs: None,
            streaming: false,
            fields: Vec::new(),
        }))
    }

    fn opens_on(choice: &Choice) -> String {
        let (engine, _) = choice().expect("a session can open");
        engine.manifest().id.clone()
    }

    #[test]
    fn each_use_opens_its_sessions_on_the_engine_chosen_for_it() {
        let points = Points::default();
        let _held: Vec<_> = ["alpha", "beta", "gamma"]
            .into_iter()
            .map(|id| points.engines.serve(stub(id)))
            .collect();
        let mut cfg = Config::default();
        cfg.recognition.set_engine(EngineRole::Dictation, "alpha");
        cfg.recognition.set_engine(EngineRole::Continuous, "beta");
        cfg.recognition.set_engine(EngineRole::Remote, "gamma");
        let config = Arc::new(Mutex::new(cfg));
        let words = std::env::temp_dir().join("anya-no-hotwords.txt");
        let dictation = choice(
            config.clone(),
            points.clone(),
            words.clone(),
            EngineRole::Dictation,
            None,
        );
        let continuous = choice(
            config.clone(),
            points.clone(),
            words.clone(),
            EngineRole::Continuous,
            None,
        );
        let remote = choice(config.clone(), points, words, EngineRole::Remote, None);
        assert_eq!(opens_on(&dictation), "alpha");
        assert_eq!(opens_on(&continuous), "beta");
        assert_eq!(opens_on(&remote), "gamma");

        // A saved change applies to the next session of that use only.
        lock(&config)
            .recognition
            .set_engine(EngineRole::Remote, "alpha");
        assert_eq!(opens_on(&remote), "alpha");
        assert_eq!(opens_on(&dictation), "alpha");
        assert_eq!(opens_on(&continuous), "beta");

        // An engine that is not served fails that use alone.
        lock(&config)
            .recognition
            .set_engine(EngineRole::Continuous, "nope");
        assert!(continuous().is_err());
        assert_eq!(opens_on(&dictation), "alpha");
    }
}
