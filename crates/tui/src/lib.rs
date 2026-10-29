//! The terminal UI: Anya's settings, live status and activity log in a
//! terminal, for machines without a desktop window.
//!
//! [`run`] takes the terminal and reads and changes the running Anya only
//! through [`Control`], so it runs against a fake in tests. It prints nothing
//! outside its own screen.

use std::io::{Stdout, stdout};
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anya_i18n::Catalog;
use anya_runtime::Control;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

mod app;
mod form;
mod view;

#[cfg(test)]
mod tests;

/// How long to wait for a key before polling the running Anya again.
const FRAME: Duration = Duration::from_millis(100);

/// Why the terminal UI could not run.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    /// Setting up, drawing on or reading the terminal failed.
    #[error("terminal: {0}")]
    Terminal(#[from] std::io::Error),
}

/// Runs the terminal UI on this process's terminal until the user quits.
///
/// Takes the alternate screen in raw mode and gives the terminal back on
/// every way out, a panic included. Polls `control` a few times a second;
/// saves go through [`Control::save`], and refused values show beside their
/// fields in `catalog`'s language (it follows `[ui] language` once saved).
/// Quitting returns; stopping Anya is the caller's.
pub fn run(control: Arc<dyn Control>, catalog: Catalog) -> Result<(), TuiError> {
    let mut app = app::App::new(control, catalog, Box::new(open_url));
    let mut session = Session::start()?;
    loop {
        app.tick();
        session.terminal.draw(|frame| view::draw(&mut app, frame))?;
        if app.done() {
            return Ok(());
        }
        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
        {
            app.key(key);
        }
    }
}

/// The terminal is in raw mode on the alternate screen.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

/// Gives the terminal back: cooked mode, the normal screen.
fn restore() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            LeaveAlternateScreen,
            ratatui::crossterm::cursor::Show
        );
    }
}

/// The terminal while the UI owns it; dropping it gives it back.
struct Session {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Session {
    fn start() -> Result<Session, TuiError> {
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });
        enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        let setup = || -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
            execute!(stdout(), EnterAlternateScreen)?;
            let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            Ok(terminal)
        };
        match setup() {
            Ok(terminal) => Ok(Session { terminal }),
            Err(e) => {
                restore();
                Err(e.into())
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        restore();
    }
}

/// Opens `url` in the system browser without waiting for it.
fn open_url(url: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    // Reaped in the background so it leaves no zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
