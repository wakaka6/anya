//! The app side: starts the helper, hands it the latest style and view on
//! a writer thread, and starts it again when it died.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anya_core::desktop::Desktop;

use super::wire::{self, Message, READY};
use crate::{HELPER_ARG, Labels, OverlayError, Style, View};

/// How the helper is started.
#[derive(Debug, Clone)]
pub(super) struct Launch {
    pub(super) program: PathBuf,
    pub(super) args: Vec<String>,
    /// How long AppKit may take to come up.
    pub(super) ready_timeout: Duration,
    /// The least time between two starts, so a helper that dies at once is
    /// not restarted for every view.
    pub(super) restart_gap: Duration,
}

/// What the writer has not handed to the helper yet. A newer value
/// replaces an older one: views come faster than the helper draws.
#[derive(Debug, Default)]
pub(super) struct Inbox {
    style: Option<Style>,
    view: Option<View>,
    closed: bool,
}

impl Inbox {
    pub(super) fn push_style(&mut self, style: Style) {
        self.style = Some(style);
    }

    pub(super) fn push_view(&mut self, view: View) {
        self.view = Some(view);
    }

    pub(super) fn take(&mut self) -> (Option<Style>, Option<View>) {
        (self.style.take(), self.view.take())
    }

    fn is_empty(&self) -> bool {
        self.style.is_none() && self.view.is_none()
    }
}

struct Shared {
    inbox: Mutex<Inbox>,
    wake: Condvar,
}

impl Shared {
    fn inbox(&self) -> MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The overlay as the app holds it.
pub(crate) struct Backend {
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
}

impl Backend {
    pub(crate) fn start(
        desktop: Desktop,
        style: Style,
        labels: Labels,
    ) -> Result<Backend, OverlayError> {
        if desktop != Desktop::Darwin {
            return Err(OverlayError::Backend(format!(
                "the macOS overlay cannot run on {desktop}"
            )));
        }
        let program = std::env::current_exe()
            .map_err(|e| OverlayError::Backend(format!("cannot find the overlay helper: {e}")))?;
        Backend::with_launch(
            Launch {
                program,
                args: vec![HELPER_ARG.to_owned()],
                ready_timeout: Duration::from_secs(5),
                restart_gap: Duration::from_secs(1),
            },
            style,
            labels,
        )
    }

    pub(super) fn with_launch(
        launch: Launch,
        style: Style,
        labels: Labels,
    ) -> Result<Backend, OverlayError> {
        let mut writer = Writer {
            launch,
            helper: None,
            style,
            labels,
            view: View::Idle,
            spawned: Instant::now(),
            stale: false,
        };
        writer.spawn()?;
        let shared = Arc::new(Shared {
            inbox: Mutex::new(Inbox::default()),
            wake: Condvar::new(),
        });
        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("anya-overlay".into())
            .spawn(move || writer.run(&thread_shared))
            .map_err(|e| OverlayError::Backend(format!("cannot start the overlay writer: {e}")))?;
        Ok(Backend {
            shared,
            writer: Some(handle),
        })
    }

    pub(crate) fn show(&self, view: View) {
        self.shared.inbox().push_view(view);
        self.shared.wake.notify_one();
    }

    pub(crate) fn restyle(&self, style: Style) {
        self.shared.inbox().push_style(style);
        self.shared.wake.notify_one();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shared.inbox().closed = true;
        self.shared.wake.notify_one();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Owns the helper; runs on its own thread.
struct Writer {
    launch: Launch,
    helper: Option<Helper>,
    style: Style,
    labels: Labels,
    view: View,
    /// When the helper was last started.
    spawned: Instant,
    /// A view the helper has not received because it is dead.
    stale: bool,
}

impl Writer {
    fn run(mut self, shared: &Shared) {
        loop {
            let (style, view, closed) = {
                let mut inbox = shared.inbox();
                loop {
                    if inbox.closed {
                        let (style, view) = inbox.take();
                        break (style, view, true);
                    }
                    if !inbox.is_empty() {
                        let (style, view) = inbox.take();
                        break (style, view, false);
                    }
                    if self.stale {
                        let wait = self
                            .launch
                            .restart_gap
                            .saturating_sub(self.spawned.elapsed());
                        if wait.is_zero() {
                            break (None, None, false);
                        }
                        inbox = shared
                            .wake
                            .wait_timeout(inbox, wait)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    } else {
                        inbox = shared.wake.wait(inbox).unwrap_or_else(|e| e.into_inner());
                    }
                }
            };
            if let Some(style) = style {
                self.restyle(style);
            }
            if let Some(view) = view {
                self.view = view;
                self.stale = true;
            }
            if closed {
                // The last view still reaches a live helper; none is started.
                if self.stale
                    && let Some(helper) = &mut self.helper
                {
                    let _ = helper.send(&wire::view(&self.view));
                }
                return;
            }
            if self.stale {
                self.deliver();
            }
        }
    }

    /// A dead helper gets the new style when it starts again.
    fn restyle(&mut self, style: Style) {
        self.style = style;
        if let Some(helper) = &mut self.helper
            && helper.send(&wire::style(&self.style)).is_err()
        {
            self.helper = None;
        }
    }

    /// Hands the latest view to the helper, starting it again if it died
    /// and the restart gap has passed.
    fn deliver(&mut self) {
        if let Some(helper) = &mut self.helper {
            if helper.alive() && helper.send(&wire::view(&self.view)).is_ok() {
                self.stale = false;
                return;
            }
            tracing::warn!("overlay helper stopped; starting it again");
            self.helper = None;
        }
        if self.spawned.elapsed() < self.launch.restart_gap {
            return;
        }
        // Success or not, the next attempt waits for the next view.
        self.stale = false;
        if let Err(e) = self.spawn() {
            tracing::warn!("overlay helper did not start again: {e}");
        }
    }

    /// Starts the helper and greets it with the style, labels and view.
    fn spawn(&mut self) -> Result<(), OverlayError> {
        self.spawned = Instant::now();
        let mut helper = Helper::launch(&self.launch)?;
        let greet = |h: &mut Helper| -> std::io::Result<()> {
            h.send(&wire::style(&self.style))?;
            h.send(&wire::labels(&self.labels))?;
            h.send(&wire::view(&self.view))
        };
        greet(&mut helper)
            .map_err(|e| OverlayError::Backend(format!("overlay helper stopped reading: {e}")))?;
        self.helper = Some(helper);
        Ok(())
    }
}

/// A running helper process.
struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    buf: Vec<u8>,
}

impl Helper {
    /// Starts the helper and waits until it says AppKit is up.
    fn launch(launch: &Launch) -> Result<Helper, OverlayError> {
        let mut child = Command::new(&launch.program)
            .args(&launch.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                OverlayError::Backend(format!(
                    "cannot start the overlay helper {}: {e}",
                    launch.program.display()
                ))
            })?;
        let (ready_tx, ready_rx) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            let _ = thread::Builder::new()
                .name("anya-overlay-out".into())
                .spawn(move || read_stdout(stdout, &ready_tx));
        }
        if let Some(stderr) = child.stderr.take() {
            let _ = thread::Builder::new()
                .name("anya-overlay-err".into())
                .spawn(move || read_stderr(stderr));
        }
        let stdin = child.stdin.take();
        let mut helper = Helper {
            child,
            stdin,
            buf: Vec::new(),
        };
        match ready_rx.recv_timeout(launch.ready_timeout) {
            Ok(()) => Ok(helper),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = helper.child.kill();
                Err(OverlayError::Backend(format!(
                    "overlay helper did not start within {:?}",
                    launch.ready_timeout
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = helper.wait_briefly();
                Err(OverlayError::Backend(match status {
                    Some(status) => format!("overlay helper exited before it was ready ({status})"),
                    None => "overlay helper closed its output before it was ready".to_owned(),
                }))
            }
        }
    }

    fn send(&mut self, message: &Message<'_>) -> std::io::Result<()> {
        let Some(stdin) = &mut self.stdin else {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        };
        self.buf.clear();
        wire::encode(&mut self.buf, message)?;
        stdin.write_all(&self.buf)?;
        stdin.flush()
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The exit status, if the helper ends within a second.
    fn wait_briefly(&mut self) -> Option<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => return None,
            }
        }
    }
}

impl Drop for Helper {
    /// Closing stdin tells the helper to quit; one that does not is killed.
    fn drop(&mut self) {
        self.stdin = None;
        if self.wait_briefly().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn read_stdout(stdout: impl std::io::Read, ready: &mpsc::Sender<()>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if line.trim() == READY {
            let _ = ready.send(());
        } else {
            tracing::debug!(target: "anya_overlay::helper", "{line}");
        }
    }
}

fn read_stderr(stderr: impl std::io::Read) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        if !line.trim().is_empty() {
            tracing::warn!(target: "anya_overlay::helper", "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;
    use crate::darwin::wire::{Update, decode};

    fn style() -> Style {
        Style {
            position: Position::Notch,
            scale: 1.0,
            idle_visible: false,
            show_text: true,
        }
    }

    fn labels() -> Labels {
        Labels {
            listening: "Listening…".into(),
            recognizing: "Recognizing…".into(),
            idle: "Anya".into(),
        }
    }

    fn listening(level: f32, text: &str) -> View {
        View::Listening {
            level,
            text: text.into(),
        }
    }

    /// A fresh directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "anya-overlay-{}-{name}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A shell standing in for the helper.
    fn shell(script: &str, restart_gap: Duration) -> Launch {
        Launch {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            ready_timeout: Duration::from_secs(5),
            restart_gap,
        }
    }

    fn lines(path: &std::path::Path) -> Vec<Update> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        // Only whole lines: the helper may be halfway through writing one.
        let whole = text.rfind('\n').map_or("", |end| &text[..end]);
        whole.lines().map(|l| decode(l).unwrap()).collect()
    }

    fn wait_for(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_inbox_keeps_the_latest_of_each() {
        let mut inbox = Inbox::default();
        inbox.push_view(listening(0.1, "a"));
        inbox.push_view(listening(0.2, "ab"));
        inbox.push_style(Style {
            scale: 2.0,
            ..style()
        });
        inbox.push_view(View::Finishing { text: "ab".into() });
        let (s, v) = inbox.take();
        assert_eq!(s.map(|s| s.scale), Some(2.0));
        assert_eq!(v, Some(View::Finishing { text: "ab".into() }));
        assert_eq!(inbox.take(), (None, None));
    }

    #[test]
    fn the_helper_is_greeted_then_gets_views_and_styles_in_order() {
        let dir = scratch("greet");
        let out = dir.join("out");
        let script = format!("echo ready; exec cat > '{}'", out.display());
        let backend =
            Backend::with_launch(shell(&script, Duration::ZERO), style(), labels()).unwrap();
        backend.show(listening(0.3, "hello"));
        let bigger = Style {
            scale: 1.5,
            ..style()
        };
        backend.restyle(bigger.clone());
        backend.show(View::Done {
            text: "hello".into(),
            label: "Copied".into(),
        });
        drop(backend);

        let got = lines(&out);
        assert_eq!(got[0], Update::Style(style()));
        assert_eq!(got[1], Update::Labels(labels()));
        assert_eq!(got[2], Update::View(View::Idle));
        assert_eq!(
            got.last(),
            Some(&Update::View(View::Done {
                text: "hello".into(),
                label: "Copied".into(),
            }))
        );
        let restyled = got.iter().position(|u| *u == Update::Style(bigger.clone()));
        assert!(restyled.is_some_and(|i| i < got.len() - 1), "{got:?}");
    }

    #[test]
    fn views_sent_faster_than_the_helper_reads_are_coalesced() {
        let dir = scratch("coalesce");
        let out = dir.join("out");
        // The helper reads nothing for a while: the pipe fills, the writer
        // blocks, and the views pile up in the inbox. The pause stays well
        // inside the second a closing helper gets before it is killed.
        let script = format!("echo ready; sleep 0.3; exec cat > '{}'", out.display());
        let backend =
            Backend::with_launch(shell(&script, Duration::ZERO), style(), labels()).unwrap();
        let padding = "字".repeat(1000);
        let total = 500;
        for i in 0..total {
            backend.show(listening(0.5, &format!("{padding}{i}")));
        }
        drop(backend);

        let got = lines(&out);
        assert!(
            got.len() < total + 3,
            "{} lines: nothing coalesced",
            got.len()
        );
        assert_eq!(
            got.last(),
            Some(&Update::View(listening(
                0.5,
                &format!("{padding}{}", total - 1)
            ))),
            "the latest view wins"
        );
    }

    #[test]
    fn a_dead_helper_is_started_again_on_the_next_view() {
        let dir = scratch("restart");
        let out = dir.join("out");
        let spawns = dir.join("spawns");
        // Each helper reads the greeting and one view, then exits.
        let script = format!(
            "echo spawn >> '{}'; echo ready; head -n 4 >> '{}'",
            spawns.display(),
            out.display()
        );
        let backend =
            Backend::with_launch(shell(&script, Duration::ZERO), style(), labels()).unwrap();
        backend.show(listening(0.2, "first"));
        wait_for(|| lines(&out).len() == 4);
        thread::sleep(Duration::from_millis(200));

        backend.show(listening(0.9, "second"));
        // The second helper writes its lines when stdin closes.
        let spawned = || {
            std::fs::read_to_string(&spawns)
                .unwrap_or_default()
                .lines()
                .count()
        };
        wait_for(|| spawned() == 2);
        thread::sleep(Duration::from_millis(200));
        drop(backend);

        let count = std::fs::read_to_string(&spawns).unwrap().lines().count();
        assert_eq!(count, 2);
        let got = lines(&out);
        assert_eq!(got[3], Update::View(listening(0.2, "first")));
        assert_eq!(got[4], Update::Style(style()));
        assert_eq!(got[5], Update::Labels(labels()));
        assert_eq!(got[6], Update::View(listening(0.9, "second")));
    }

    #[test]
    fn a_helper_that_never_gets_ready_is_an_error() {
        let mut launch = shell("exec sleep 30", Duration::ZERO);
        launch.ready_timeout = Duration::from_millis(200);
        let started = Instant::now();
        let err = Backend::with_launch(launch, style(), labels())
            .err()
            .unwrap();
        assert!(err.to_string().contains("did not start"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_helper_that_exits_at_once_is_an_error() {
        let err = Backend::with_launch(shell("exit 3", Duration::ZERO), style(), labels())
            .err()
            .unwrap();
        assert!(
            err.to_string().contains("exited before it was ready"),
            "{err}"
        );
    }

    #[test]
    fn other_desktops_are_refused() {
        for desktop in [Desktop::Wayland, Desktop::X11] {
            let err = Backend::start(desktop, style(), labels()).err().unwrap();
            assert!(err.to_string().contains("cannot run on"), "{err}");
        }
    }
}
