//! The Bun host process: started, fed commands, read for events and, when it
//! dies, restarted with backoff. A death ends the sessions it ran with an
//! error; nothing retries them elsewhere (docs/decisions/0005).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anya_core::engine::{Engine, EngineError, Output};
use anya_protocol::{ErrorCode, HostCommand, HostEvent, LogLevel, Manifest, PLUGIN_PROTOCOL};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::engine::PluginEngine;
use crate::{BunPath, PluginDirs, PluginError, Store};

/// The host script, written next to the SDK into the runtime directory.
const HOST_JS: &str = include_str!("../../../host/host.js");

/// The SDK the host serves to plugins as `@wakaka6/anya-plugin`.
const SDK_JS: &str = include_str!("../../../sdk/plugin/index.js");

/// Commands waiting for the host's stdin: 40 ms of audio each, so about
/// forty seconds of one session. A host that falls further behind than that
/// ends the sessions that cannot be served.
pub(crate) const COMMAND_QUEUE: usize = 1024;

/// Results queued per session for the hub, which drains them at once.
pub(crate) const OUTPUT_QUEUE: usize = 64;

/// How long a new host may take to say `ready`.
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a host gets to exit after `shutdown`.
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// The first and longest wait before restarting a host that died.
const BACKOFF_START: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// A host that ran this long counts as healthy: its next death starts the
/// backoff over.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// Where a plugin is on its way to being usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// The host was asked to load it and has not answered.
    Loading,
    /// It loaded and its engine can open sessions.
    Ready,
    /// It did not load; carries the reason, untranslated.
    Failed(String),
}

/// One enabled plugin as the host sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatus {
    /// The plugin id.
    pub id: String,
    /// Whether it loaded.
    pub state: PluginState,
    /// The engine it provides, once it has loaded; kept while the host
    /// restarts.
    pub manifest: Option<Manifest>,
}

/// What the supervisor is asked to do.
enum Control {
    /// Restart the host with the plugins that are enabled now.
    Reload,
    /// Stop the host and end.
    Shutdown,
}

#[derive(Default)]
struct State {
    /// Where commands go while a host runs.
    writer: Option<mpsc::Sender<HostCommand>>,
    /// Open sessions by host session id.
    sessions: HashMap<String, mpsc::Sender<Output>>,
    status: Vec<PluginStatus>,
}

/// What the supervisor, the engines and their sessions share.
pub(crate) struct Shared {
    state: Mutex<State>,
    next: AtomicU64,
    changes: watch::Sender<u64>,
}

impl Shared {
    fn new() -> Arc<Shared> {
        Arc::new(Shared {
            state: Mutex::default(),
            next: AtomicU64::new(1),
            changes: watch::channel(0).0,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-update; the state stays usable.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n += 1);
    }

    /// Registers a session and returns its id and the writer to the host, or
    /// `None` while no host runs.
    pub(crate) fn register(
        &self,
        output: mpsc::Sender<Output>,
    ) -> Option<(String, mpsc::Sender<HostCommand>)> {
        let mut state = self.state();
        let writer = state.writer.clone()?;
        let id = format!("s{}", self.next.fetch_add(1, Ordering::Relaxed));
        state.sessions.insert(id.clone(), output);
        Some((id, writer))
    }

    /// Forgets a session; true when it was still open.
    pub(crate) fn take_session(&self, id: &str) -> bool {
        self.state().sessions.remove(id).is_some()
    }

    /// Ends a session from this side with `error`.
    pub(crate) fn fail_session(&self, id: &str, error: EngineError) {
        if let Some(output) = self.state().sessions.remove(id) {
            let _ = output.try_send(Output::Error(error));
        }
    }

    /// Ends every session: the host they ran in is gone.
    fn fail_all(&self, reason: &str) {
        let sessions: Vec<_> = {
            let mut state = self.state();
            state.writer = None;
            state.sessions.drain().collect()
        };
        for (_, output) in sessions {
            let _ = output.try_send(Output::Error(EngineError::Upstream(reason.to_owned())));
        }
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

/// The plugin host: owns the Bun process that runs the enabled engine
/// plugins. Dropping it stops the process.
pub struct PluginHost {
    shared: Arc<Shared>,
    store: Store,
    control: mpsc::Sender<Control>,
    task: Option<JoinHandle<()>>,
}

impl PluginHost {
    /// Starts supervising a host for the plugins enabled in `dirs`, using
    /// `bun`. Returns at once; plugins load in the background (see
    /// [`PluginHost::settled`]). With no plugin enabled no process runs.
    /// Must be called inside a Tokio runtime.
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
    /// one while the host is down ends with an error at once.
    pub fn engines(&self) -> Vec<Arc<dyn Engine>> {
        let state = self.shared.state();
        state
            .status
            .iter()
            .filter_map(|s| {
                let manifest = s.manifest.clone()?;
                let engine: Arc<dyn Engine> = Arc::new(PluginEngine::new(
                    s.id.clone(),
                    manifest,
                    Arc::clone(&self.shared),
                ));
                Some(engine)
            })
            .collect()
    }

    /// Every enabled plugin and whether it loaded.
    pub fn status(&self) -> Vec<PluginStatus> {
        self.shared.state().status.clone()
    }

    /// Whether a host process is up and takes commands.
    pub fn running(&self) -> bool {
        self.shared.state().writer.is_some()
    }

    /// A counter that changes whenever [`PluginHost::status`] or
    /// [`PluginHost::running`] might have.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.shared.changes.subscribe()
    }

    /// Waits until the host runs and no plugin is still loading, or until
    /// `timeout`. Returns whether that happened.
    pub async fn settled(&self, timeout: Duration) -> bool {
        let mut changes = self.changes();
        let settled = || {
            let state = self.shared.state();
            let wanted = self.store.enabled().map_or(0, |w| w.len());
            (wanted == 0 || state.writer.is_some())
                && state.status.len() == wanted
                && state.status.iter().all(|s| s.state != PluginState::Loading)
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

    /// Restarts the host with the plugins enabled now, after an enable,
    /// disable, remove or update. Open sessions end with an error.
    pub fn reload(&self) {
        let _ = self.control.try_send(Control::Reload);
    }

    /// Stops the host process and waits for it.
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

/// How one run of the host ended.
enum Exit {
    Shutdown,
    Reload,
    Died(String),
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
        let wanted = store.enabled().unwrap_or_else(|e| {
            tracing::warn!("no plugins load: {e}");
            Vec::new()
        });
        shared.state().status = wanted
            .iter()
            .map(|(p, _)| PluginStatus {
                id: p.id.clone(),
                state: PluginState::Loading,
                manifest: None,
            })
            .collect();
        shared.changed();
        let started = Instant::now();
        let exit = if wanted.is_empty() {
            match controls.recv().await {
                Some(Control::Reload) => Exit::Reload,
                Some(Control::Shutdown) | None => Exit::Shutdown,
            }
        } else {
            run(&shared, &dirs, &bun, &wanted, &mut controls).await
        };
        shared.fail_all("the plugin host stopped");
        shared.changed();
        match exit {
            Exit::Shutdown => return,
            Exit::Reload => backoff = BACKOFF_START,
            Exit::Died(reason) => {
                tracing::warn!("plugin host died: {reason}; restarting in {backoff:?}");
                if started.elapsed() >= HEALTHY_AFTER {
                    backoff = BACKOFF_START;
                }
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    control = controls.recv() => match control {
                        Some(Control::Reload) => {}
                        Some(Control::Shutdown) | None => return,
                    },
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Runs one host process until it exits or is told to stop.
async fn run(
    shared: &Arc<Shared>,
    dirs: &PluginDirs,
    bun: &BunPath,
    wanted: &[(crate::Installed, std::path::PathBuf)],
    controls: &mut mpsc::Receiver<Control>,
) -> Exit {
    let script = match write_runtime(&dirs.runtime).await {
        Ok(script) => script,
        Err(e) => return Exit::Died(e.to_string()),
    };
    let mut child = match Command::new(&bun.0)
        .arg(&script)
        .current_dir(&dirs.runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Exit::Died(format!("cannot start {}: {e}", bun.0.display())),
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

    let exit = 'session: {
        match tokio::time::timeout(READY_TIMEOUT, ready(&mut lines)).await {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => break 'session Exit::Died(reason),
            Err(_) => break 'session Exit::Died("the host did not become ready".to_owned()),
        }
        shared.state().writer = Some(writer.clone());
        shared.changed();
        for (plugin, dir) in wanted {
            let load = HostCommand::Load {
                plugin: plugin.id.clone(),
                path: dir.display().to_string(),
            };
            if writer.send(load).await.is_err() {
                break 'session Exit::Died("the host closed its input".to_owned());
            }
        }
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => handle_line(shared, &writer, &line),
                    Ok(None) => break Exit::Died("the host exited".to_owned()),
                    Err(e) => break Exit::Died(format!("reading the host failed: {e}")),
                },
                control = controls.recv() => match control {
                    Some(Control::Reload) => break Exit::Reload,
                    Some(Control::Shutdown) | None => break Exit::Shutdown,
                },
            }
        }
    };

    if !matches!(exit, Exit::Died(_)) {
        let _ = tokio::time::timeout(EXIT_GRACE, writer.send(HostCommand::Shutdown)).await;
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

/// Lets the host exit after `shutdown`, then makes sure it has.
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

fn handle_line(shared: &Arc<Shared>, writer: &mpsc::Sender<HostCommand>, line: &str) {
    let event = match serde_json::from_str::<HostEvent>(line) {
        Ok(event) => event,
        Err(e) => {
            tracing::debug!("skipped a host line that is not an event: {e}");
            return;
        }
    };
    match event {
        HostEvent::Loaded { plugin, manifest } => loaded(shared, &plugin, manifest),
        HostEvent::LoadFailed { plugin, detail } => {
            tracing::warn!(%plugin, "plugin did not load: {detail}");
            shared.set_status(&plugin, |s| {
                s.state = PluginState::Failed(detail);
                s.manifest = None;
            });
        }
        HostEvent::SessionReady { session } => {
            send_output(shared, writer, &session, Output::Ready, false);
        }
        HostEvent::Recognition {
            session,
            recognition,
        } => {
            let last = recognition.is_final;
            send_output(shared, writer, &session, Output::Result(recognition), last);
        }
        HostEvent::SessionError {
            session,
            code,
            detail,
        } => {
            shared.fail_session(&session, engine_error(code, detail));
        }
        HostEvent::SessionClosed { session } => {
            shared.take_session(&session);
        }
        HostEvent::Log {
            plugin,
            level,
            message,
        } => log(&plugin, level, &message),
        HostEvent::Ready { .. } | HostEvent::Unknown => {}
    }
}

/// Delivers `output` to the session; `last` forgets the session after it.
/// A session whose hub side is gone or full is closed on the host.
fn send_output(
    shared: &Arc<Shared>,
    writer: &mpsc::Sender<HostCommand>,
    session: &str,
    output: Output,
    last: bool,
) {
    let mut state = shared.state();
    let Some(tx) = state.sessions.get(session) else {
        return;
    };
    let delivered = tx.try_send(output).is_ok();
    if last || !delivered {
        state.sessions.remove(session);
    }
    drop(state);
    if !delivered {
        tracing::warn!(session, "session reader is gone or too slow; closing it");
        let _ = writer.try_send(HostCommand::Close {
            session: session.to_owned(),
        });
    }
}

fn loaded(shared: &Arc<Shared>, plugin: &str, manifest: Manifest) {
    let taken_by = shared
        .state()
        .status
        .iter()
        .find(|s| s.id != plugin && s.manifest.as_ref().is_some_and(|m| m.id == manifest.id))
        .map(|s| s.id.clone());
    shared.set_status(plugin, |s| match taken_by {
        Some(other) => {
            s.state = PluginState::Failed(format!(
                "engine id {:?} is already provided by plugin {other}",
                manifest.id
            ));
            s.manifest = None;
        }
        None => {
            s.state = PluginState::Ready;
            s.manifest = Some(manifest);
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
async fn write_runtime(runtime: &std::path::Path) -> Result<std::path::PathBuf, PluginError> {
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
