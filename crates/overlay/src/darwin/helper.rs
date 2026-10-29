//! The helper process: AppKit on the main thread, lines from stdin on a
//! reader thread. The reader keeps only the latest of each kind of line and
//! wakes the main queue once; the main thread applies them.

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use super::ca;
use super::capsule::Capsule;
use super::notch::Notch;
use super::wire::{self, READY, Update};
use crate::{Labels, Position, Style, View};

/// How long a finished session's text stays before the overlay goes idle.
const DONE_HOLD: Duration = Duration::from_millis(1200);

pub(crate) fn helper_main() -> ! {
    let Some(mtm) = MainThreadMarker::new() else {
        let _ = writeln!(
            std::io::stderr(),
            "the overlay helper must run on the main thread"
        );
        std::process::exit(1);
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    if std::thread::Builder::new()
        .name("anya-overlay-stdin".into())
        .spawn(read_stdin)
        .is_err()
    {
        let _ = writeln!(std::io::stderr(), "cannot start the overlay stdin reader");
        std::process::exit(1);
    }
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{READY}").and_then(|()| out.flush()).is_err() {
        std::process::exit(1);
    }
    drop(out);
    app.run();
    std::process::exit(0);
}

fn read_stdin() {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match wire::decode(line) {
            Ok(update) => push(|p| match update {
                Update::Style(s) => p.style = Some(s),
                Update::Labels(l) => p.labels = Some(l),
                Update::View(v) => p.view = Some(v),
            }),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "{e}");
            }
        }
    }
    // The app closed stdin or died: the overlay goes with it.
    push(|p| p.closed = true);
}

/// What the main thread has not applied yet.
#[derive(Default)]
struct Pending {
    style: Option<Style>,
    labels: Option<Labels>,
    view: Option<View>,
    closed: bool,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    style: None,
    labels: None,
    view: None,
    closed: false,
});
static SCHEDULED: AtomicBool = AtomicBool::new(false);

fn push(f: impl FnOnce(&mut Pending)) {
    f(&mut PENDING.lock().unwrap_or_else(|e| e.into_inner()));
    if !SCHEDULED.swap(true, Ordering::AcqRel) {
        DispatchQueue::main().exec_async(drain);
    }
}

fn drain() {
    SCHEDULED.store(false, Ordering::Release);
    let pending = std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()));
    if pending.closed {
        std::process::exit(0);
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    UI.with(|cell| {
        let Ok(mut ui) = cell.try_borrow_mut() else {
            return;
        };
        if ui.is_none() {
            // The app sends the style first; anything before it waits.
            let Some(style) = pending.style.clone() else {
                push(|p| {
                    p.labels = p.labels.take().or(pending.labels);
                    p.view = p.view.take().or(pending.view);
                });
                return;
            };
            *ui = Some(Ui::new(mtm, style));
        }
        let Some(ui) = ui.as_mut() else { return };
        if let Some(style) = pending.style {
            ui.restyle(style);
        }
        if let Some(labels) = pending.labels {
            ui.labels = labels;
            ui.render();
        }
        if let Some(view) = pending.view {
            ui.show(view);
        }
    });
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

static TOKENS: AtomicU64 = AtomicU64::new(1);

/// A number no earlier timer holds: a timer whose token is no longer
/// current does nothing.
pub(super) fn token() -> u64 {
    TOKENS.fetch_add(1, Ordering::Relaxed)
}

/// What a timer does when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tick {
    /// The done hold is over.
    Hold,
    /// One frame of the meter and clock.
    Frame,
    /// One step of the typewriter.
    Type,
    /// The next step of collapsing the notch.
    Collapse(u8),
}

/// Fires `tick` with `token` on the main thread after `delay`.
pub(super) fn schedule(delay: Duration, token: u64, tick: Tick) {
    ca::after(delay, move || {
        UI.with(|cell| {
            if let Ok(mut ui) = cell.try_borrow_mut()
                && let Some(ui) = ui.as_mut()
            {
                ui.tick(token, tick);
            }
        });
    });
}

/// The surface for a position: the notch, or the capsule elsewhere.
enum Surface {
    Notch(Box<Notch>),
    Capsule(Box<Capsule>),
}

impl Surface {
    fn new(mtm: MainThreadMarker, style: &Style) -> Surface {
        if style.position == Position::Notch {
            Surface::Notch(Box::new(Notch::new(mtm, style.scale)))
        } else {
            Surface::Capsule(Box::new(Capsule::new(mtm, style.scale)))
        }
    }

    fn fits(&self, style: &Style, scale: f64) -> bool {
        let notch = matches!(self, Surface::Notch(_));
        notch == (style.position == Position::Notch) && scale == style.scale
    }

    fn hide(&mut self) {
        match self {
            Surface::Notch(n) => n.hide(),
            Surface::Capsule(c) => c.hide(),
        }
    }
}

/// The helper's state, on the main thread.
struct Ui {
    mtm: MainThreadMarker,
    style: Style,
    labels: Labels,
    /// The latest view the app sent.
    view: View,
    /// The done hold of the current [`View::Done`] is over: it shows idle.
    done_over: bool,
    hold: u64,
    surface: Surface,
}

impl Ui {
    fn new(mtm: MainThreadMarker, style: Style) -> Ui {
        let surface = Surface::new(mtm, &style);
        Ui {
            mtm,
            style,
            labels: Labels::default(),
            view: View::Idle,
            done_over: false,
            hold: 0,
            surface,
        }
    }

    fn restyle(&mut self, style: Style) {
        let scale = self.style.scale;
        self.style = style;
        if !self.surface.fits(&self.style, scale) {
            // The old window goes before the new one shows.
            self.surface.hide();
            self.surface = Surface::new(self.mtm, &self.style);
        }
        self.render();
    }

    fn show(&mut self, view: View) {
        if let View::Done { .. } = view {
            if matches!(self.view, View::Done { .. }) {
                // The same session's result again: update in place, and
                // never reopen a session whose hold is over.
                self.view = view;
                if !self.done_over {
                    self.render();
                }
                return;
            }
            self.hold = token();
            schedule(DONE_HOLD, self.hold, Tick::Hold);
        }
        self.view = view;
        self.done_over = false;
        self.render();
    }

    /// What the surface draws: idle once a done hold is over.
    fn shown(&self) -> &View {
        if self.done_over {
            &View::Idle
        } else {
            &self.view
        }
    }

    fn render(&mut self) {
        let view = if self.done_over {
            View::Idle
        } else {
            self.view.clone()
        };
        match &mut self.surface {
            Surface::Notch(n) => n.show(&view, &self.style, &self.labels),
            Surface::Capsule(c) => c.show(&view, &self.style, &self.labels),
        }
    }

    fn tick(&mut self, token: u64, tick: Tick) {
        if tick == Tick::Hold {
            if token == self.hold && matches!(self.shown(), View::Done { .. }) {
                self.done_over = true;
                self.render();
            }
            return;
        }
        match &mut self.surface {
            Surface::Notch(n) => n.tick(token, tick),
            Surface::Capsule(_) => {}
        }
    }
}
