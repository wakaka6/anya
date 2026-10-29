//! One tray app per user and config file.
//!
//! The first app to start holds a file lock in the runtime directory and
//! listens on a Unix socket beside it. A later start with the same config
//! finds the lock taken, sends `show` over the socket so the first one shows
//! its settings window, and exits. The lock goes with the process, so a
//! crashed app never blocks the next start.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const REQUEST: &str = "show";
const REPLY: &str = "ok";

/// How long a later start waits for the first one to answer; the first one
/// may still be between taking the lock and listening.
const ANSWER_WITHIN: Duration = Duration::from_secs(3);

/// What claiming found.
pub(crate) enum Claim {
    /// No app ran: this process holds the lock and serves activations.
    Primary(Instance),
    /// Another app runs and was asked to show its settings window.
    Activated,
}

/// Why claiming failed.
#[derive(Debug)]
pub(crate) struct ClaimError {
    pub(crate) path: PathBuf,
    pub(crate) source: io::Error,
}

/// The stable name of a config file's instance: a hash of its canonical
/// path. A file that does not exist yet is named by its canonical parent
/// directory and its file name, so it keeps the name once it is written.
pub(crate) fn key(config_path: &Path) -> String {
    format!(
        "{:016x}",
        fnv1a(canonical(config_path).as_os_str().as_encoded_bytes())
    )
}

fn canonical(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    if let Ok(found) = fs::canonicalize(&absolute) {
        return found;
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => fs::canonicalize(parent)
            .map(|p| p.join(name))
            .unwrap_or(absolute),
        _ => absolute,
    }
}

/// FNV-1a, 64 bit: stable across builds, unlike the standard hasher.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Takes the lock for `key` in `dir`, or asks the app holding it to show
/// its settings window.
pub(crate) fn claim(dir: &Path, key: &str) -> Result<Claim, ClaimError> {
    let failed = |path: &Path| {
        let path = path.to_owned();
        move |source| ClaimError { path, source }
    };
    fs::create_dir_all(dir).map_err(failed(dir))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(failed(dir))?;
    let lock_path = dir.join(format!("app-{key}.lock"));
    let socket = dir.join(format!("app-{key}.sock"));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(failed(&lock_path))?;
    match lock.try_lock() {
        Ok(()) => Instance::listen(lock, socket.clone())
            .map(Claim::Primary)
            .map_err(failed(&socket)),
        Err(TryLockError::WouldBlock) => activate(&socket)
            .map(|()| Claim::Activated)
            .map_err(failed(&socket)),
        Err(TryLockError::Error(e)) => Err(failed(&lock_path)(e)),
    }
}

/// Asks the app listening on `socket` to show its settings window.
fn activate(socket: &Path) -> io::Result<()> {
    let deadline = Instant::now() + ANSWER_WITHIN;
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    };
    let left = deadline.saturating_duration_since(Instant::now());
    stream.set_read_timeout(Some(left.max(Duration::from_millis(100))))?;
    stream.write_all(format!("{REQUEST}\n").as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.trim() == REPLY {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected answer {:?}", reply.trim()),
        ))
    }
}

type Handler = Box<dyn Fn() + Send>;

/// Activations waiting for the window, or the window's handler.
#[derive(Default)]
struct Activations {
    inner: Mutex<(Option<Handler>, bool)>,
}

impl Activations {
    fn fire(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        match &inner.0 {
            Some(handler) => handler(),
            None => inner.1 = true,
        }
    }

    fn set(&self, handler: Option<Handler>) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handler) = &handler
            && std::mem::take(&mut inner.1)
        {
            handler();
        }
        inner.0 = handler;
    }
}

/// The lock and the socket of the first app. Dropping it stops listening,
/// removes the socket and releases the lock.
pub(crate) struct Instance {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    activations: Arc<Activations>,
    thread: Option<JoinHandle<()>>,
    // Released last, after the socket is gone.
    _lock: File,
}

impl Instance {
    fn listen(lock: File, socket: PathBuf) -> io::Result<Instance> {
        // The lock is ours, so a socket file left here is from a process
        // that ended without removing it.
        match fs::remove_file(&socket) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let stop = Arc::new(AtomicBool::new(false));
        let activations = Arc::new(Activations::default());
        let thread = thread::Builder::new()
            .name("anya-instance".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                let activations = Arc::clone(&activations);
                move || serve(&listener, &stop, &activations)
            })?;
        Ok(Instance {
            socket,
            stop,
            activations,
            thread: Some(thread),
            _lock: lock,
        })
    }

    /// Calls `handler` (on the listening thread) for every later start, and
    /// at once for one that came before it was set.
    pub(crate) fn on_activate(&self, handler: impl Fn() + Send + 'static) {
        self.activations.set(Some(Box::new(handler)));
    }
}

fn serve(listener: &UnixListener, stop: &AtomicBool, activations: &Activations) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(stream) = stream else { continue };
        if let Err(e) = answer(stream, activations) {
            tracing::debug!(error = %e, "single-instance request failed");
        }
    }
}

fn answer(stream: UnixStream, activations: &Activations) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    if line.trim() != REQUEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown request {:?}", line.trim()),
        ));
    }
    activations.fire();
    (&stream).write_all(format!("{REPLY}\n").as_bytes())
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.activations.set(None);
        self.stop.store(true, Ordering::Release);
        // Wakes the accept loop so it sees the stop flag.
        drop(UnixStream::connect(&self.socket));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!("anya-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn primary(dir: &Path, key: &str) -> Instance {
        match claim(dir, key) {
            Ok(Claim::Primary(instance)) => instance,
            Ok(Claim::Activated) => panic!("expected to be first"),
            Err(e) => panic!("claim failed: {:?}: {}", e.path, e.source),
        }
    }

    fn activated(dir: &Path, key: &str) -> bool {
        matches!(claim(dir, key), Ok(Claim::Activated))
    }

    #[test]
    fn a_second_start_asks_the_first_to_show_its_window() {
        let dir = TempDir::new("inst-show");
        let first = primary(&dir.0, "k");
        let (tx, rx) = mpsc::channel();
        first.on_activate(move || {
            let _ = tx.send(());
        });
        let second = thread::spawn({
            let dir = dir.0.clone();
            move || activated(&dir, "k")
        });
        assert!(second.join().is_ok_and(|activated| activated));
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
    }

    #[test]
    fn a_start_before_the_window_is_ready_is_kept_until_it_is() {
        let dir = TempDir::new("inst-early");
        let first = primary(&dir.0, "k");
        assert!(activated(&dir.0, "k"));
        let (tx, rx) = mpsc::channel();
        first.on_activate(move || {
            let _ = tx.send(());
        });
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn the_lock_is_free_again_once_the_first_app_ends() {
        let dir = TempDir::new("inst-free");
        let first = primary(&dir.0, "k");
        drop(first);
        assert!(!dir.0.join("app-k.sock").exists());
        let _again = primary(&dir.0, "k");
    }

    #[test]
    fn a_socket_left_by_a_crash_does_not_block_the_next_start() {
        let dir = TempDir::new("inst-stale");
        fs::create_dir_all(&dir.0).ok();
        let stale = UnixListener::bind(dir.0.join("app-k.sock"));
        drop(stale);
        let _first = primary(&dir.0, "k");
        assert!(activated(&dir.0, "k"));
    }

    #[test]
    fn different_configs_run_side_by_side() {
        let dir = TempDir::new("inst-two");
        let _a = primary(&dir.0, "a");
        let _b = primary(&dir.0, "b");
    }

    #[test]
    fn the_runtime_directory_is_private() {
        let dir = TempDir::new("inst-mode");
        let _first = primary(&dir.0, "k");
        let mode = |p: &Path| fs::metadata(p).map(|m| m.permissions().mode() & 0o777).ok();
        assert_eq!(mode(&dir.0), Some(0o700));
        assert_eq!(mode(&dir.0.join("app-k.sock")), Some(0o600));
    }

    #[test]
    fn a_config_keeps_its_key_once_the_file_is_written() {
        let dir = TempDir::new("inst-key");
        fs::create_dir_all(&dir.0).ok();
        let path = dir.0.join("config.toml");
        let before = key(&path);
        fs::write(&path, "").ok();
        assert_eq!(key(&path), before);
        assert_eq!(key(&dir.0.join(".").join("config.toml")), before);
        assert_ne!(key(&dir.0.join("other.toml")), before);
    }

    #[test]
    fn the_key_is_stable_across_builds() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
