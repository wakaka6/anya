//! The plugin host: one Bun process per enabled package that has a host
//! entry, each started, fed commands, read for events, watched and, when it
//! dies or stops answering, restarted with backoff. A death ends the sessions
//! it ran with an error; nothing retries them elsewhere
//! (docs/decisions/0005, docs/decisions/0015).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anya_core::engine::{Engine, EngineError, Output};
use anya_core::hooks::{ActionHook, ContextHook, Extra, HookError, TransformHook};
use anya_protocol::{
    ErrorCode, HostCommand, HostEvent, LogLevel, Manifest, PLUGIN_PROTOCOL, Provide,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::engine::PluginEngine;
use crate::hooks::HostHook;
use crate::link::{Answer, COMMAND_QUEUE, Link};
use crate::store::Hosted;
use crate::{BunPath, PluginDirs, PluginError, Store};

/// The host script, written next to the SDK into the runtime directory.
const HOST_JS: &str = include_str!("../../../host/host.js");

/// The SDK the host serves to plugins as `@wakaka6/anya-plugin`.
const SDK_JS: &str = include_str!("../../../sdk/plugin/index.js");

/// How long a new process may take to say `ready`.
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a process gets to exit after `shutdown`.
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// How often a ready process is pinged.
const PING_EVERY: Duration = Duration::from_secs(1);

/// How long a ready process may send nothing at all before it is killed.
/// Fixed: a longer wait would only keep a stuck plugin alive (0015).
const SILENCE: Duration = Duration::from_secs(3);

/// The first and longest wait before restarting a process that died.
const BACKOFF_START: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// A process that ran this long counts as healthy: its next death starts the
/// backoff over.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// Plugin ids the watchdog killed, buffered per subscriber.
const STALL_BUFFER: usize = 16;

/// Why a session on a killed process ended.
const STALLED: &str = "the plugin stopped answering";

/// Why a session on a process that died ended.
const STOPPED: &str = "the plugin host stopped";

/// Where a plugin is on its way to being usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// The host was asked to load it and has not answered.
    Loading,
    /// It loaded and what it provides can be used.
    Ready,
    /// It did not load; carries the reason, untranslated.
    Failed(String),
}

/// One enabled plugin with a host entry, as the host sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatus {
    /// The plugin id.
    pub id: String,
    /// Whether it loaded.
    pub state: PluginState,
    /// The engine it provides, once it has loaded; kept while its process
    /// restarts.
    pub manifest: Option<Manifest>,
    /// Everything its host entry provides, once it has loaded.
    pub provides: Vec<Provide>,
}

/// What the supervisor is asked to do.
enum Control {
    /// Restart every process with the plugins that are enabled now.
    Reload,
    /// Stop every process and end.
    Shutdown,
}

#[derive(Default)]
struct State {
    status: Vec<PluginStatus>,
    links: Vec<Arc<Link>>,
}

/// What the supervisor, the child tasks and the host's methods share.
pub(crate) struct Shared {
    state: Mutex<State>,
    changes: watch::Sender<u64>,
    stalls: broadcast::Sender<String>,
}

impl Shared {
    fn new() -> Arc<Shared> {
        Arc::new(Shared {
            state: Mutex::default(),
            changes: watch::channel(0).0,
            stalls: broadcast::channel(STALL_BUFFER).0,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-update; the state stays usable.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n += 1);
    }

    fn set_status(&self, id: &str, update: impl FnOnce(&mut PluginStatus)) {
        {
            let mut state = self.state();
            if let Some(entry) = state.status.iter_mut().find(|s| s.id == id) {
                update(entry);
            }
        }
        self.changed();
    }
}

/// The plugin host: owns the Bun processes that run the enabled plugins'
/// host entries. Dropping it stops them.
pub struct PluginHost {
    shared: Arc<Shared>,
    store: Store,
    control: mpsc::Sender<Control>,
    task: Option<JoinHandle<()>>,
}

impl PluginHost {
    /// Starts supervising a Bun process for every plugin enabled in `dirs`
    /// that has a host entry, using `bun`. Plugins with only an inline entry
    /// start no process. Returns at once; plugins load in the background
    /// (see [`PluginHost::settled`]). Must be called inside a Tokio runtime.
    pub fn start(dirs: PluginDirs, bun: BunPath) -> PluginHost {
        let shared = Shared::new();
        let store = Store::new(dirs.clone());
        let (control, controls) = mpsc::channel(4);
        let task = tokio::spawn(supervise(
            Arc::clone(&shared),
            store.clone(),
            dirs,
            bun,
            controls,
        ));
        PluginHost {
            shared,
            store,
            control,
            task: Some(task),
        }
    }

    /// The installed plugins this host loads from.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The engines of the plugins that loaded, one per plugin. A session on
    /// one while its process is down ends with an error at once.
    pub fn engines(&self) -> Vec<Arc<dyn Engine>> {
        let state = self.shared.state();
        state
            .status
            .iter()
            .filter_map(|s| {
                let manifest = s.manifest.clone()?;
                let link = state.links.iter().find(|l| l.plugin() == s.id)?;
                let engine: Arc<dyn Engine> =
                    Arc::new(PluginEngine::new(s.id.clone(), manifest, Arc::clone(link)));
                Some(engine)
            })
            .collect()
    }

    /// The plugins that loaded and provide `hook`, as hooks.
    fn hooks(&self, hook: Provide) -> Vec<Arc<HostHook>> {
        let state = self.shared.state();
        state
            .status
            .iter()
            .filter(|s| s.state == PluginState::Ready && s.provides.contains(&hook))
            .filter_map(|s| {
                let link = state.links.iter().find(|l| l.plugin() == s.id)?;
                Some(Arc::new(HostHook::new(Arc::clone(link))))
            })
            .collect()
    }

    /// The context hooks of the plugins that loaded, one per plugin. A call
    /// while the plugin's process is down fails as unavailable.
    pub fn contexts(&self) -> Vec<Arc<dyn ContextHook>> {
        self.hooks(Provide::Context)
            .into_iter()
            .map(|h| h as Arc<dyn ContextHook>)
            .collect()
    }

    /// The transform hooks of the plugins that loaded, one per plugin.
    pub fn transforms(&self) -> Vec<Arc<dyn TransformHook>> {
        self.hooks(Provide::Transform)
            .into_iter()
            .map(|h| h as Arc<dyn TransformHook>)
            .collect()
    }

    /// The action hooks of the plugins that loaded, one per plugin.
    pub fn actions(&self) -> Vec<Arc<dyn ActionHook>> {
        self.hooks(Provide::Action)
            .into_iter()
            .map(|h| h as Arc<dyn ActionHook>)
            .collect()
    }

    /// Every enabled plugin with a host entry and whether it loaded.
    pub fn status(&self) -> Vec<PluginStatus> {
        self.shared.state().status.clone()
    }

    /// Whether any plugin process is up and takes commands.
    pub fn running(&self) -> bool {
        self.shared.state().links.iter().any(|l| l.is_up())
    }

    /// The ids of plugins the watchdog killed for not answering, as it kills
    /// them. A receiver that falls behind misses the oldest.
    pub fn stalls(&self) -> broadcast::Receiver<String> {
        self.shared.stalls.subscribe()
    }

    /// A counter that changes whenever [`PluginHost::status`] or
    /// [`PluginHost::running`] might have.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.shared.changes.subscribe()
    }

    /// Waits until every plugin that has a host entry is up and none is
    /// still loading, or until `timeout`. Returns whether that happened.
    pub async fn settled(&self, timeout: Duration) -> bool {
        let mut changes = self.changes();
        let settled = || {
            let wanted = self.store.hosted().map_or(0, |w| w.len());
            let state = self.shared.state();
            state.status.len() == wanted
                && state.status.iter().all(|s| match s.state {
                    PluginState::Loading => false,
                    PluginState::Failed(_) => true,
                    PluginState::Ready => {
                        state.links.iter().any(|l| l.plugin() == s.id && l.is_up())
                    }
                })
        };
        tokio::time::timeout(timeout, async {
            while !settled() {
                if changes.changed().await.is_err() {
                    return;
                }
            }
        })
        .await
        .is_ok()
    }

    /// Restarts the processes with the plugins enabled now, after an enable,
    /// disable, remove or update. Open sessions end with an error.
    pub fn reload(&self) {
        let _ = self.control.try_send(Control::Reload);
    }

    /// Stops the plugin processes and waits for them.
    pub async fn shutdown(mut self) {
        let _ = self.control.send(Control::Shutdown).await;
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        let _ = self.control.try_send(Control::Shutdown);
    }
}

/// How one run of a plugin's process ended.
enum Exit {
    Stop,
    Died(String),
    Stalled,
}

async fn supervise(
    shared: Arc<Shared>,
    store: Store,
    dirs: PluginDirs,
    bun: BunPath,
    mut controls: mpsc::Receiver<Control>,
) {
    let mut backoff = BACKOFF_START;
    loop {
        // An unreadable record loads nothing; it is left as it is for the
        // user to fix, and the next reload reads it again.
        let hosted = store.hosted().unwrap_or_else(|e| {
            tracing::warn!("no plugins load: {e}");
            Vec::new()
        });
        let script = if hosted.iter().any(|h| h.package.is_ok()) {
            match write_runtime(&dirs.runtime).await {
                Ok(script) => Some(script),
                Err(e) => {
                    tracing::warn!("cannot write the plugin runtime: {e}; retrying in {backoff:?}");
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {}
                        control = controls.recv() => match control {
                            Some(Control::Reload) => backoff = BACKOFF_START,
                            Some(Control::Shutdown) | None => return,
                        },
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                    continue;
                }
            }
        } else {
            None
        };
        backoff = BACKOFF_START;

        let (stop, stopped) = watch::channel(false);
        let mut links = Vec::new();
        let mut status = Vec::new();
        let mut children = Vec::new();
        for Hosted {
            installed,
            dir,
            package,
        } in hosted
        {
            let link = Arc::new(Link::new(installed.id.clone()));
            let mut entry = PluginStatus {
                id: installed.id.clone(),
                state: PluginState::Loading,
                manifest: None,
                provides: Vec::new(),
            };
            match (package, &script) {
                (Ok(_), Some(script)) => {
                    children.push(tokio::spawn(child(
                        Arc::clone(&shared),
                        Arc::clone(&link),
                        Spawn {
                            dir,
                            bun: bun.clone(),
                            script: script.clone(),
                            runtime: dirs.runtime.clone(),
                        },
                        stopped.clone(),
                    )));
                }
                (Err(e), _) => {
                    tracing::warn!(plugin = %installed.id, "plugin did not load: {e}");
                    entry.state = PluginState::Failed(e.to_string());
                }
                (Ok(_), None) => {}
            }
            links.push(link);
            status.push(entry);
        }
        {
            let mut state = shared.state();
            state.status = status;
            state.links = links.clone();
        }
        shared.changed();

        let control = controls.recv().await;
        let _ = stop.send(true);
        for task in children {
            let _ = task.await;
        }
        for link in &links {
            link.fail_all(STOPPED);
        }
        shared.changed();
        match control {
            Some(Control::Reload) => {}
            Some(Control::Shutdown) | None => return,
        }
    }
}

/// What a child task needs to start its process.
struct Spawn {
    dir: PathBuf,
    bun: BunPath,
    script: PathBuf,
    runtime: PathBuf,
}

/// Runs one plugin's process for as long as `stopped` allows: start, watch,
/// and when it dies or stalls restart it after a backoff.
async fn child(
    shared: Arc<Shared>,
    link: Arc<Link>,
    spawn: Spawn,
    mut stopped: watch::Receiver<bool>,
) {
    let mut backoff = BACKOFF_START;
    loop {
        shared.set_status(link.plugin(), |s| {
            s.state = PluginState::Loading;
            s.manifest = None;
            s.provides = Vec::new();
        });
        let started = Instant::now();
        let exit = run(&shared, &link, &spawn, &mut stopped).await;
        link.fail_all(if matches!(exit, Exit::Stalled) {
            STALLED
        } else {
            STOPPED
        });
        shared.changed();
        match exit {
            Exit::Stop => return,
            Exit::Died(reason) => tracing::warn!(
                plugin = link.plugin(),
                "plugin host died: {reason}; restarting in {backoff:?}"
            ),
            Exit::Stalled => {
                tracing::warn!(
                    plugin = link.plugin(),
                    "plugin stopped answering and was killed; restarting in {backoff:?}"
                );
                let _ = shared.stalls.send(link.plugin().to_owned());
            }
        }
        if started.elapsed() >= HEALTHY_AFTER {
            backoff = BACKOFF_START;
        }
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            _ = stopped.changed() => return,
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Runs one process of one plugin until it exits, stalls or is told to stop.
async fn run(
    shared: &Arc<Shared>,
    link: &Arc<Link>,
    spawn: &Spawn,
    stopped: &mut watch::Receiver<bool>,
) -> Exit {
    let mut child = match Command::new(&spawn.bun.0)
        .arg(&spawn.script)
        .current_dir(&spawn.runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Exit::Died(format!("cannot start {}: {e}", spawn.bun.0.display())),
    };
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Exit::Died("the host has no stdio".to_owned());
    };
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "anya_plugin_host::stderr", "{line}");
        }
    });

    let (writer, commands) = mpsc::channel(COMMAND_QUEUE);
    let write_task = tokio::spawn(write_commands(stdin, commands));
    let mut lines = BufReader::new(stdout).lines();

    let exit = 'watch: {
        let ready = tokio::select! {
            ready = tokio::time::timeout(READY_TIMEOUT, ready(&mut lines)) => ready,
            _ = stopped.changed() => break 'watch Exit::Stop,
        };
        match ready {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => break 'watch Exit::Died(reason),
            Err(_) => break 'watch Exit::Died("the host did not become ready".to_owned()),
        }
        link.set_writer(writer.clone());
        shared.changed();
        let load = HostCommand::Load {
            plugin: link.plugin().to_owned(),
            path: spawn.dir.display().to_string(),
        };
        if writer.send(load).await.is_err() {
            break 'watch Exit::Died("the host closed its input".to_owned());
        }

        // The watchdog: a ping every second; a process that has not answered
        // one for SILENCE is stuck, however much else it writes (a plugin
        // looping on console.log never gets to answer).
        let mut last = Instant::now();
        let mut seq = 0u64;
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if handle_line(shared, link, &writer, &line) {
                            last = Instant::now();
                        }
                    }
                    Ok(None) => break Exit::Died("the host exited".to_owned()),
                    Err(e) => break Exit::Died(format!("reading the host failed: {e}")),
                },
                _ = ping.tick() => {
                    seq += 1;
                    let _ = writer.try_send(HostCommand::Ping { seq });
                }
                () = tokio::time::sleep_until(last + SILENCE) => break Exit::Stalled,
                _ = stopped.changed() => break Exit::Stop,
            }
        }
    };

    match exit {
        Exit::Stop => {
            let _ = tokio::time::timeout(EXIT_GRACE, writer.send(HostCommand::Shutdown)).await;
        }
        Exit::Stalled => {
            let _ = child.start_kill();
        }
        Exit::Died(_) => {}
    }
    drop(writer);
    stop(&mut child, write_task).await;
    exit
}

/// Waits for the host's `ready` line.
async fn ready(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Result<(), String> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|e| format!("reading the host failed: {e}"))?
            .ok_or("the host exited before it was ready")?;
        match serde_json::from_str::<HostEvent>(&line) {
            Ok(HostEvent::Ready { protocol, .. }) if protocol == PLUGIN_PROTOCOL => return Ok(()),
            Ok(HostEvent::Ready { protocol, .. }) => {
                return Err(format!(
                    "the host speaks plugin protocol {protocol}, Anya {PLUGIN_PROTOCOL}"
                ));
            }
            _ => {}
        }
    }
}

/// Lets the process exit after `shutdown`, then makes sure it has.
async fn stop(child: &mut Child, writer: JoinHandle<()>) {
    if tokio::time::timeout(EXIT_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    writer.abort();
}

async fn write_commands(
    mut stdin: tokio::process::ChildStdin,
    mut commands: mpsc::Receiver<HostCommand>,
) {
    while let Some(command) = commands.recv().await {
        let Ok(mut line) = serde_json::to_vec(&command) else {
            continue;
        };
        line.push(b'\n');
        if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
            return;
        }
    }
}

/// Acts on one line of the host; true when it answered a ping.
fn handle_line(
    shared: &Arc<Shared>,
    link: &Arc<Link>,
    writer: &mpsc::Sender<HostCommand>,
    line: &str,
) -> bool {
    let event = match serde_json::from_str::<HostEvent>(line) {
        Ok(event) => event,
        Err(e) => {
            tracing::debug!("skipped a host line that is not an event: {e}");
            return false;
        }
    };
    match event {
        HostEvent::Loaded {
            engine, provides, ..
        } => loaded(shared, link.plugin(), engine, provides),
        HostEvent::LoadFailed { detail, .. } => {
            let plugin = link.plugin();
            tracing::warn!(%plugin, "plugin did not load: {detail}");
            shared.set_status(plugin, |s| {
                s.state = PluginState::Failed(detail);
                s.manifest = None;
                s.provides = Vec::new();
            });
        }
        HostEvent::SessionReady { session } => {
            send_output(link, writer, &session, Output::Ready, false);
        }
        HostEvent::Recognition {
            session,
            recognition,
        } => {
            let last = recognition.is_final;
            send_output(link, writer, &session, Output::Result(recognition), last);
        }
        HostEvent::SessionError {
            session,
            code,
            detail,
        } => {
            link.fail_session(&session, engine_error(code, detail));
        }
        HostEvent::SessionClosed { session } => {
            link.take_session(&session);
        }
        HostEvent::Log {
            plugin,
            level,
            message,
        } => log(&plugin, level, &message),
        HostEvent::ContextDone {
            call,
            hotwords,
            language,
        } => link.finish_call(&call, Ok(Answer::Context(Extra { hotwords, language }))),
        HostEvent::Transformed { call, text } => link.finish_call(&call, Ok(Answer::Text(text))),
        HostEvent::ActionDone { call } => link.finish_call(&call, Ok(Answer::Done)),
        HostEvent::CallFailed { call, detail } => {
            link.finish_call(&call, Err(HookError::Failed(detail)));
        }
        HostEvent::Pong { .. } => return true,
        HostEvent::Ready { .. } | HostEvent::Unknown => {}
    }
    false
}

/// Delivers `output` to the session; `last` forgets the session after it.
/// A session whose hub side is gone or full is closed on the host.
fn send_output(
    link: &Arc<Link>,
    writer: &mpsc::Sender<HostCommand>,
    session: &str,
    output: Output,
    last: bool,
) {
    if link.deliver(session, output, last) == Some(false) {
        tracing::warn!(session, "session reader is gone or too slow; closing it");
        let _ = writer.try_send(HostCommand::Close {
            session: session.to_owned(),
        });
    }
}

fn loaded(shared: &Arc<Shared>, plugin: &str, engine: Option<Manifest>, provides: Vec<Provide>) {
    let taken_by = engine.as_ref().and_then(|manifest| {
        shared
            .state()
            .status
            .iter()
            .find(|s| s.id != plugin && s.manifest.as_ref().is_some_and(|m| m.id == manifest.id))
            .map(|s| s.id.clone())
    });
    shared.set_status(plugin, |s| match (taken_by, engine) {
        (Some(other), Some(manifest)) => {
            s.state = PluginState::Failed(format!(
                "engine id {:?} is already provided by plugin {other}",
                manifest.id
            ));
            s.manifest = None;
            s.provides = Vec::new();
        }
        (_, engine) => {
            s.state = PluginState::Ready;
            s.manifest = engine;
            s.provides = provides;
        }
    });
}

fn engine_error(code: ErrorCode, detail: String) -> EngineError {
    match code {
        ErrorCode::NotConfigured => EngineError::NotConfigured(detail),
        ErrorCode::Connect => EngineError::Connect(detail),
        _ => EngineError::Upstream(detail),
    }
}

/// Plugin output can carry transcripts and credentials, so it stays at debug.
fn log(plugin: &str, level: LogLevel, message: &str) {
    tracing::debug!(target: "anya_plugin_host::plugin", %plugin, ?level, "{message}");
}

/// Writes the host script and the SDK it serves; returns the script's path.
async fn write_runtime(runtime: &Path) -> Result<PathBuf, PluginError> {
    tokio::fs::create_dir_all(runtime)
        .await
        .map_err(|e| PluginError::io(runtime, &e))?;
    for (name, text) in [("host.js", HOST_JS), ("sdk.js", SDK_JS)] {
        let path = runtime.join(name);
        let current = tokio::fs::read_to_string(&path).await.ok();
        if current.as_deref() != Some(text) {
            tokio::fs::write(&path, text)
                .await
                .map_err(|e| PluginError::io(&path, &e))?;
        }
    }
    Ok(runtime.join("host.js"))
}
