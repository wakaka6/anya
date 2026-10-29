//! The Linux overlay: a wlr-layer-shell surface on Wayland, an
//! override-redirect window on X11, both drawn in software.
//!
//! One thread owns the display connection and everything on it. `show` and
//! `restyle` leave the latest view and style in a shared slot and wake that
//! thread; it reads the slot, advances the [`model::Model`], and draws when
//! the scene changed. Shutting down is the same thread tearing down its own
//! objects, so a closing app never races the compositor connection.

mod font;
mod layout;
mod model;
mod paint;
mod wayland;
mod x11;

use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

use anya_core::desktop::Desktop;
use anya_core::doctor::{Check, Severity};
use smithay_client_toolkit::reexports::calloop::ping::{Ping, make_ping};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle};

use self::font::Fonts;
use self::layout::{anchor, normalize_scale};
use self::model::{Model, Scene};
use self::paint::Painter;
use crate::{Labels, OverlayError, Style, View};

/// What the app asked for since the overlay thread last looked.
#[derive(Default)]
struct Inbox {
    view: Option<View>,
    style: Option<Style>,
    quit: bool,
}

type Shared = Arc<Mutex<Inbox>>;
type Ready = mpsc::Sender<Result<Ping, OverlayError>>;

/// The running Linux overlay.
pub(crate) struct Backend {
    inbox: Shared,
    wake: Ping,
    thread: Option<JoinHandle<()>>,
}

impl Backend {
    /// Connects to `desktop`'s display server on a thread of its own and
    /// returns once the window system accepted the overlay or refused it.
    pub(crate) fn start(
        desktop: Desktop,
        style: Style,
        labels: Labels,
    ) -> Result<Backend, OverlayError> {
        if desktop == Desktop::Darwin {
            return Err(OverlayError::Backend(
                "the Linux overlay cannot run on macOS".into(),
            ));
        }
        spawn(style, labels, move |driver, ready| {
            open(desktop, driver, ready)
        })
    }

    pub(crate) fn show(&self, view: View) {
        self.post(|inbox| inbox.view = Some(view));
    }

    pub(crate) fn restyle(&self, style: Style) {
        self.post(|inbox| inbox.style = Some(style));
    }

    fn post(&self, f: impl FnOnce(&mut Inbox)) {
        f(&mut self.inbox.lock().unwrap_or_else(PoisonError::into_inner));
        self.wake.ping();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.post(|inbox| inbox.quit = true);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the overlay thread panicked");
        }
    }
}

/// Starts the overlay thread: it loads the fonts, then `body` opens a
/// window system and runs it, reporting on `ready` whether it could.
fn spawn(
    style: Style,
    labels: Labels,
    body: impl FnOnce(Driver, Ready) + Send + 'static,
) -> Result<Backend, OverlayError> {
    let inbox = Shared::default();
    let shared = Arc::clone(&inbox);
    let (ready, started) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("anya-overlay".into())
        .spawn(move || match Fonts::system() {
            Ok(fonts) => body(
                Driver::new(shared, style, labels, Painter::new(fonts)),
                ready,
            ),
            Err(e) => {
                let _ = ready.send(Err(e));
            }
        })
        .map_err(|e| OverlayError::Backend(format!("cannot start the overlay thread: {e}")))?;
    let failed = |thread: JoinHandle<()>, e| {
        let _ = thread.join();
        Err(e)
    };
    match started.recv() {
        Ok(Ok(wake)) => Ok(Backend {
            inbox,
            wake,
            thread: Some(thread),
        }),
        Ok(Err(e)) => failed(thread, e),
        Err(_) => failed(
            thread,
            OverlayError::Backend("the overlay thread ended while starting".into()),
        ),
    }
}

fn display_set() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
}

/// Opens the window system for `desktop` and runs it. A Wayland compositor
/// without layer-shell falls back to XWayland when there is one.
fn open(desktop: Desktop, driver: Driver, ready: Ready) {
    let x11 = |driver, ready: Ready| match x11::X11::open() {
        Ok(win) => run(win, driver, ready),
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    };
    match desktop {
        Desktop::Wayland => match wayland::Wayland::open() {
            Ok(win) => run(win, driver, ready),
            Err(e) if display_set() => {
                tracing::warn!("{e}; the overlay uses XWayland");
                x11(driver, ready);
            }
            Err(e) => {
                let _ = ready.send(Err(e));
            }
        },
        Desktop::X11 => x11(driver, ready),
        Desktop::Darwin => {
            let _ = ready.send(Err(OverlayError::Backend(
                "the Linux overlay cannot run on macOS".into(),
            )));
        }
    }
}

/// The overlay's state on its thread, apart from the window system.
struct Driver {
    inbox: Shared,
    model: Model,
    style: Style,
    labels: Labels,
    painter: Painter,
}

impl Driver {
    fn new(inbox: Shared, mut style: Style, labels: Labels, painter: Painter) -> Driver {
        style.scale = normalize_scale(style.scale);
        Driver {
            inbox,
            model: Model::new(Instant::now()),
            style,
            labels,
            painter,
        }
    }

    /// Applies what the app sent. Returns whether to quit, and whether the
    /// window must be built again for a new place or size.
    fn take(&mut self, now: Instant) -> (bool, bool) {
        let (view, style, quit) = {
            let mut inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
            (inbox.view.take(), inbox.style.take(), inbox.quit)
        };
        if let Some(view) = view {
            self.model.show(view, now);
        }
        let mut rebuild = false;
        if let Some(mut style) = style {
            style.scale = normalize_scale(style.scale);
            rebuild = needs_rebuild(&self.style, &style);
            self.style = style;
        }
        (quit, rebuild)
    }
}

/// Whether going from `old` to `new` moves or resizes the window. The
/// notch and top center are the same place here.
fn needs_rebuild(old: &Style, new: &Style) -> bool {
    anchor(old.position) != anchor(new.position) || old.scale != new.scale
}

/// The loop's data: the window system and the driver.
struct Host<W> {
    win: W,
    driver: Driver,
}

/// A window system the overlay draws on.
trait Window: Sized + 'static {
    /// Registers the connection's events with the loop.
    fn watch(&mut self, handle: &LoopHandle<'static, Host<Self>>) -> Result<(), OverlayError>;
    /// Whether the window system asked for a redraw since last asked.
    fn take_dirty(&mut self) -> bool;
    /// Draws `scene`, or hides the window for `None`.
    fn present(&mut self, scene: Option<&Scene>, style: &Style, painter: &mut Painter);
    /// Drops the window, so the next present builds it for the new style.
    fn rebuild(&mut self);
    /// Whether the connection still works.
    fn alive(&self) -> bool;
    /// Tears everything down before the thread ends.
    fn close(&mut self);
}

/// Runs `win` until the app drops the overlay or the connection breaks.
fn run<W: Window>(win: W, driver: Driver, ready: Ready) {
    let fail = |e: String| {
        let _ = ready.send(Err(OverlayError::Backend(e)));
    };
    let mut event_loop: EventLoop<'static, Host<W>> = match EventLoop::try_new() {
        Ok(event_loop) => event_loop,
        Err(e) => return fail(format!("cannot create the overlay event loop: {e}")),
    };
    let (wake, woken) = match make_ping() {
        Ok(ping) => ping,
        Err(e) => return fail(format!("cannot create the overlay wake-up: {e}")),
    };
    if let Err(e) = event_loop.handle().insert_source(woken, |_, _, _| {}) {
        return fail(format!("cannot watch the overlay wake-up: {}", e.error));
    }
    let mut host = Host { win, driver };
    if let Err(e) = host.win.watch(&event_loop.handle()) {
        let _ = ready.send(Err(e));
        return;
    }
    if ready.send(Ok(wake)).is_err() {
        host.win.close();
        return;
    }

    // What the window shows; `None` until the first present.
    let mut shown: Option<Option<Scene>> = None;
    loop {
        let now = Instant::now();
        let (quit, rebuild) = host.driver.take(now);
        if quit {
            break;
        }
        if rebuild {
            host.win.rebuild();
            shown = None;
        }
        host.driver.model.tick(now);
        let d = &mut host.driver;
        let scene = d.model.scene(&d.style, &d.labels, now);
        let dirty = host.win.take_dirty();
        if dirty || shown.as_ref() != Some(&scene) {
            host.win.present(scene.as_ref(), &d.style, &mut d.painter);
            shown = Some(scene);
        }
        if !host.win.alive() {
            tracing::warn!("the overlay lost its display connection");
            break;
        }
        let timeout = host.driver.model.next_wake(Instant::now());
        if let Err(e) = event_loop.dispatch(timeout, &mut host) {
            tracing::warn!("the overlay's event loop failed: {e}");
            break;
        }
    }
    host.win.close();
}

/// What the overlay needs from the host on `desktop`: a display it can
/// draw on, a font, and a font with Chinese characters.
pub(crate) fn checks(desktop: Desktop) -> Vec<Check> {
    let display = match desktop {
        Desktop::Darwin => return Vec::new(),
        Desktop::Wayland => wayland_check(),
        Desktop::X11 => x11_check(),
    };
    let font = match font::sans_serif() {
        Some(path) => Check::passed("overlay_font", Severity::Warning, "found").with("font", path),
        None => Check::failed("overlay_font", Severity::Warning, "none"),
    };
    let cjk = match font::chinese() {
        Some(path) => {
            Check::passed("overlay_cjk_font", Severity::Warning, "found").with("font", path)
        }
        None => Check::failed("overlay_cjk_font", Severity::Warning, "none"),
    };
    vec![display, font, cjk]
}

const DISPLAY_CHECK: &str = "overlay_display";

fn wayland_check() -> Check {
    let name = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    match wayland::probe() {
        Some(true) => Check::passed(DISPLAY_CHECK, Severity::Warning, "layer_shell"),
        Some(false) if display_set() && x11::probe() => {
            Check::passed(DISPLAY_CHECK, Severity::Warning, "xwayland")
        }
        Some(false) => Check::failed(DISPLAY_CHECK, Severity::Warning, "no_layer_shell"),
        None => Check::failed(DISPLAY_CHECK, Severity::Warning, "no_wayland").with("display", name),
    }
}

fn x11_check() -> Check {
    let name = std::env::var("DISPLAY").unwrap_or_default();
    if name.is_empty() {
        Check::failed(DISPLAY_CHECK, Severity::Warning, "no_display")
    } else if x11::probe() {
        Check::passed(DISPLAY_CHECK, Severity::Warning, "x11").with("display", name)
    } else {
        Check::failed(DISPLAY_CHECK, Severity::Warning, "x11_refused").with("display", name)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::model::{DONE_HOLD, Icon};
    use super::paint::Canvas;
    use super::*;
    use crate::Position;

    fn style(position: Position, scale: f64) -> Style {
        Style {
            position,
            scale,
            idle_visible: false,
            show_text: true,
        }
    }

    fn labels() -> Labels {
        Labels {
            listening: "Listening…".into(),
            recognizing: "Recognizing…".into(),
            idle: "Idle".into(),
        }
    }

    #[test]
    fn the_notch_is_top_center_and_only_place_or_size_rebuilds() {
        let base = style(Position::TopCenter, 1.0);
        assert!(!needs_rebuild(&base, &style(Position::Notch, 1.0)));
        assert!(needs_rebuild(&base, &style(Position::TopLeft, 1.0)));
        assert!(needs_rebuild(&base, &style(Position::TopCenter, 1.5)));
        let quiet = Style {
            show_text: false,
            idle_visible: true,
            ..base.clone()
        };
        assert!(!needs_rebuild(&base, &quiet));
    }

    #[test]
    fn macos_is_refused_with_a_typed_error() {
        let e = Backend::start(Desktop::Darwin, style(Position::TopCenter, 1.0), labels());
        assert!(matches!(e, Err(OverlayError::Backend(_))));
    }

    /// What a fake window was asked to show: the icon and text of each
    /// present (`None` for hidden), and a mark per rebuild.
    #[derive(Debug, Clone, PartialEq)]
    enum Shown {
        Scene(Icon, Option<String>),
        Hidden,
        Rebuilt,
    }

    struct Recorder(Arc<Mutex<Vec<Shown>>>);

    impl Window for Recorder {
        fn watch(&mut self, _: &LoopHandle<'static, Host<Self>>) -> Result<(), OverlayError> {
            Ok(())
        }

        fn take_dirty(&mut self) -> bool {
            false
        }

        fn present(&mut self, scene: Option<&Scene>, style: &Style, painter: &mut Painter) {
            let entry = match scene {
                Some(scene) => {
                    let mut data = vec![0; 460 * 42 * 4];
                    let mut canvas = Canvas::new(&mut data, 460, 42);
                    painter.paint(
                        scene,
                        &mut canvas,
                        style.scale as f32,
                        anchor(style.position).align,
                    );
                    Shown::Scene(scene.icon, scene.text.as_ref().map(|l| l.text.clone()))
                }
                None => Shown::Hidden,
            };
            self.0.lock().expect("log").push(entry);
        }

        fn rebuild(&mut self) {
            self.0.lock().expect("log").push(Shown::Rebuilt);
        }

        fn alive(&self) -> bool {
            true
        }

        fn close(&mut self) {}
    }

    fn wait_for(log: &Mutex<Vec<Shown>>, what: impl Fn(&[Shown]) -> bool) -> Vec<Shown> {
        let deadline = Instant::now() + DONE_HOLD * 3;
        loop {
            let seen = log.lock().expect("log").clone();
            if what(&seen) || Instant::now() > deadline {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn views_are_coalesced_drawn_on_the_thread_and_a_result_hides_itself() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder(Arc::clone(&log));
        let backend = spawn(
            style(Position::BottomCenter, 1.0),
            labels(),
            move |driver, ready| run(recorder, driver, ready),
        )
        .expect("start");

        let sent = Instant::now();
        for i in 0..200 {
            backend.show(View::Listening {
                level: 0.5,
                text: format!("word {i}"),
            });
        }
        assert!(sent.elapsed() < Duration::from_secs(1), "show blocked");
        let seen = wait_for(&log, |s| {
            s.last()
                .is_some_and(|l| matches!(l, Shown::Scene(_, Some(t)) if t == "word 199"))
        });
        assert!(
            matches!(seen.last(), Some(Shown::Scene(_, Some(t))) if t == "word 199"),
            "{seen:?}"
        );

        backend.restyle(style(Position::TopRight, 1.0));
        backend.show(View::Done {
            text: "word 199".into(),
            label: "Pasted".into(),
        });
        let done =
            |s: &Shown| matches!(s, Shown::Scene(Icon::Check(_), Some(t)) if t == "word 199");
        let seen = wait_for(&log, |s| s.last().is_some_and(done));
        let rebuilt = seen
            .iter()
            .position(|s| *s == Shown::Rebuilt)
            .expect("rebuilt");
        assert!(seen[rebuilt..].iter().any(done), "{seen:?}");
        // The app reports idle at once; the result still holds, then hides.
        let held = Instant::now();
        backend.show(View::Idle);
        let seen = wait_for(&log, |s| s.last() == Some(&Shown::Hidden));
        assert_eq!(seen.last(), Some(&Shown::Hidden), "{seen:?}");
        assert!(
            held.elapsed() >= DONE_HOLD / 2,
            "hid after {:?}",
            held.elapsed()
        );

        let closing = Instant::now();
        drop(backend);
        assert!(closing.elapsed() < Duration::from_secs(1), "drop hung");
    }
}
