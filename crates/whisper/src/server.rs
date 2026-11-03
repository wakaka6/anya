//! The whisper-server Anya runs: started on the first session that needs it,
//! on a free port of `127.0.0.1`, and stopped after it has been idle for the
//! user's time, when Anya quits, or when another model is wanted.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

/// The program looked for when the user names none.
pub(crate) const PROGRAM: &str = "whisper-server";

/// Where Homebrew puts programs; a desktop app's `PATH` often lacks them.
const PREFIXES: [&str; 3] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/home/linuxbrew/.linuxbrew/bin",
];

/// How long loading a model may take before the start counts as failed.
const START_TIMEOUT: Duration = Duration::from_secs(180);

/// How often a starting server is asked whether it is ready.
const POLL: Duration = Duration::from_millis(200);

/// Most often the idle check runs.
const IDLE_CHECK: Duration = Duration::from_secs(15);

/// How long a stopped server may take to exit before it is left to the system.
const EXIT_WAIT: Duration = Duration::from_secs(5);

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The `whisper-server` program.
    pub program: PathBuf,
    /// The model file it loads.
    pub model: PathBuf,
    /// Stop after this long without a session; `None` keeps it running.
    pub idle: Option<Duration>,
}

/// The server now, as the settings show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    /// Not running; the next session that needs it starts it.
    Stopped,
    /// Started; loading its model.
    Starting {
        /// The model file.
        model: PathBuf,
    },
    /// Answering on `port`.
    Running {
        /// The model file.
        model: PathBuf,
        /// Its port on `127.0.0.1`.
        port: u16,
        /// Sessions using it now.
        sessions: usize,
        /// How long it has had none; zero while it has some.
        idle: Duration,
    },
    /// The last start failed.
    Failed {
        /// Why, in English: the program's last words or what went wrong.
        detail: String,
    },
}

/// Why the server could not be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("whisper-server did not start: {0}")]
pub struct StartError(pub String);

/// The one server Anya runs.
#[derive(Default)]
pub(crate) struct Server {
    /// Held across a start, so concurrent sessions wait for the same one.
    running: tokio::sync::Mutex<Option<Running>>,
    /// What [`Server::state`] answers without waiting for a start.
    shown: Mutex<Shown>,
}

#[derive(Default)]
enum Shown {
    #[default]
    Stopped,
    Starting(PathBuf),
    Running(Arc<Usage>, PathBuf, u16),
    Failed(String),
}

struct Running {
    child: Child,
    launch: Launch,
    url: String,
    usage: Arc<Usage>,
}

/// Sessions using a running server, and when the last one ended.
struct Usage {
    sessions: AtomicUsize,
    since: Mutex<Instant>,
}

impl Usage {
    fn idle(&self) -> Duration {
        if self.sessions.load(Ordering::SeqCst) > 0 {
            Duration::ZERO
        } else {
            lock(&self.since).elapsed()
        }
    }
}

/// A session's hold on the running server: while any is held, the server is
/// not idle.
pub(crate) struct Lease {
    pub(crate) url: String,
    usage: Arc<Usage>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        *lock(&self.usage.since) = Instant::now();
        self.usage.sessions.fetch_sub(1, Ordering::SeqCst);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `program` when it is a file, or, for an empty one, `whisper-server` on
/// `PATH` or in Homebrew's prefixes.
pub fn find_program(program: &str) -> Option<PathBuf> {
    let program = program.trim();
    if !program.is_empty() {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    let on_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    on_path
        .into_iter()
        .chain(PREFIXES.iter().map(PathBuf::from))
        .map(|dir| dir.join(PROGRAM))
        .find(|p| p.is_file())
}

impl Server {
    pub(crate) fn state(&self) -> ServerState {
        match &*lock(&self.shown) {
            Shown::Stopped => ServerState::Stopped,
            Shown::Starting(model) => ServerState::Starting {
                model: model.clone(),
            },
            Shown::Running(usage, model, port) => ServerState::Running {
                model: model.clone(),
                port: *port,
                sessions: usage.sessions.load(Ordering::SeqCst),
                idle: usage.idle(),
            },
            Shown::Failed(detail) => ServerState::Failed {
                detail: detail.clone(),
            },
        }
    }

    fn show(&self, shown: Shown) {
        *lock(&self.shown) = shown;
    }

    /// The server running `launch`, started first when it is not running or
    /// runs something else. Waits while it loads its model.
    pub(crate) async fn lease(self: &Arc<Self>, launch: Launch) -> Result<Lease, StartError> {
        let mut running = self.running.lock().await;
        if let Some(r) = running.as_mut() {
            let alive = matches!(r.child.try_wait(), Ok(None));
            if alive && r.launch.program == launch.program && r.launch.model == launch.model {
                r.launch.idle = launch.idle;
                r.usage.sessions.fetch_add(1, Ordering::SeqCst);
                return Ok(Lease {
                    url: r.url.clone(),
                    usage: Arc::clone(&r.usage),
                });
            }
        }
        if let Some(old) = running.take() {
            stop(old).await;
        }
        self.show(Shown::Starting(launch.model.clone()));
        match start(&launch).await {
            Ok((child, port)) => {
                let usage = Arc::new(Usage {
                    sessions: AtomicUsize::new(1),
                    since: Mutex::new(Instant::now()),
                });
                let url = format!("http://127.0.0.1:{port}");
                tracing::info!(port, model = %launch.model.display(), "whisper-server is ready");
                self.show(Shown::Running(
                    Arc::clone(&usage),
                    launch.model.clone(),
                    port,
                ));
                *running = Some(Running {
                    child,
                    launch,
                    url: url.clone(),
                    usage: Arc::clone(&usage),
                });
                self.watch(&usage);
                Ok(Lease { url, usage })
            }
            Err(e) => {
                tracing::warn!(error = %e, "whisper-server did not start");
                self.show(Shown::Failed(e.0.clone()));
                Err(e)
            }
        }
    }

    /// Stops the server once it has been idle for its time, or forgets it
    /// once it exits by itself. Ends when `usage`'s server is gone.
    fn watch(self: &Arc<Self>, usage: &Arc<Usage>) {
        let server = Arc::downgrade(self);
        let usage = Arc::downgrade(usage);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(IDLE_CHECK).await;
                let (Some(server), Some(usage)) = (server.upgrade(), usage.upgrade()) else {
                    return;
                };
                let mut running = server.running.lock().await;
                let Some(r) = running.as_mut().filter(|r| Arc::ptr_eq(&r.usage, &usage)) else {
                    return;
                };
                if let Ok(Some(status)) = r.child.try_wait() {
                    tracing::warn!(%status, "whisper-server exited");
                    running.take();
                    server.show(Shown::Stopped);
                    return;
                }
                if r.launch.idle.is_some_and(|idle| usage.idle() >= idle) {
                    tracing::info!("whisper-server stopped after being idle");
                    if let Some(old) = running.take() {
                        stop(old).await;
                    }
                    server.show(Shown::Stopped);
                    return;
                }
            }
        });
    }

    /// Stops the server if it runs. Sessions on it fail.
    pub(crate) async fn stop(&self) {
        if let Some(old) = self.running.lock().await.take() {
            stop(old).await;
        }
        self.show(Shown::Stopped);
    }
}

async fn stop(mut old: Running) {
    let _ = old.child.start_kill();
    let _ = tokio::time::timeout(EXIT_WAIT, old.child.wait()).await;
}

/// A port of `127.0.0.1` nothing listens on now.
fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port())
}

/// Starts `launch` and waits until it answers.
async fn start(launch: &Launch) -> Result<(Child, u16), StartError> {
    let port = free_port().map_err(|e| StartError(format!("no free port: {e}")))?;
    tracing::info!(
        program = %launch.program.display(),
        model = %launch.model.display(),
        port,
        "starting whisper-server"
    );
    let mut child = Command::new(&launch.program)
        .arg("-m")
        .arg(&launch.model)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| StartError(format!("cannot run {}: {e}", launch.program.display())))?;
    let last = Arc::new(Mutex::new(String::new()));
    let relays = [
        child.stdout.take().map(|out| relay(out, Arc::clone(&last))),
        child.stderr.take().map(|err| relay(err, Arc::clone(&last))),
    ];
    match wait_ready(&mut child, port, &launch.model).await {
        Ok(()) => Ok((child, port)),
        Err(why) => {
            let _ = child.start_kill();
            // Its last words, once it has closed its output.
            for relay in relays.into_iter().flatten() {
                let _ = tokio::time::timeout(EXIT_WAIT, relay).await;
            }
            let said = lock(&last).clone();
            Err(StartError(if said.is_empty() {
                why
            } else {
                format!("{why}: {said}")
            }))
        }
    }
}

/// Logs what the server prints, at debug level, keeping its last line.
fn relay(
    stream: impl AsyncRead + Unpin + Send + 'static,
    last: Arc<Mutex<String>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            tracing::debug!(target: "whisper-server", "{line}");
            line.clone_into(&mut lock(&last));
        }
    })
}

/// Waits until the server answers on `port`. `/health` says 503 while the
/// model loads; a server without `/health` listens only once it has loaded,
/// so any other answer means ready.
async fn wait_ready(child: &mut Child, port: u16, model: &Path) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("it exited ({status})"));
        }
        if let Ok(response) = client.get(&url).send().await
            && response.status() != StatusCode::SERVICE_UNAVAILABLE
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "it did not load {} within {} s",
                model.display(),
                START_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}
