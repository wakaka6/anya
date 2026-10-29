//! The tray app: the tray icon, its panel and the settings window.
//!
//! [`run`] shows the page served by [`crate::web`] in two webviews: the
//! settings window and a small panel that a click on the tray icon toggles.
//! [`activate_existing`] keeps to one app per user and config file.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use anya_i18n::Catalog;
use anya_runtime::Control;

mod app;
mod icons;
mod instance;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
mod placement;
mod shell;
mod toggle;

/// Why the tray app cannot run or stopped.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// No window can open here (no display, no tray, WebKit missing): the
    /// caller falls back to the terminal UI. The reason is for the log.
    #[error("no desktop window: {0}")]
    NoWindow(String),
    /// [`run`] was called off the process's main thread.
    #[error("the tray app must run on the main thread")]
    NotMainThread,
    /// The settings page could not be served.
    #[error("cannot serve the settings page: {0}")]
    Serve(#[from] crate::web::WebError),
    /// The single-instance lock or its socket failed.
    #[error("single instance at {}: {source}", path.display())]
    Instance {
        /// The lock file, socket or directory.
        path: PathBuf,
        /// What failed.
        #[source]
        source: io::Error,
    },
    /// There is no runtime directory for the single-instance lock.
    #[error("no runtime directory: {0}")]
    RuntimeDir(#[from] anya_config::ConfigError),
}

/// What reaches the event loop from other threads.
#[derive(Debug)]
enum UserEvent {
    /// The tray icon was clicked or hovered.
    Tray(tray_icon::TrayIconEvent),
    /// A tray menu item was chosen.
    Menu(tray_icon::menu::MenuEvent),
    /// Show the settings window (the page, or a later start of Anya).
    ShowSettings,
    /// The user quit from the page: end Anya.
    Quit,
    /// The binary asked the app to end.
    Exit,
    /// Open the file dialog on GTK's thread and send back the choice.
    #[cfg(target_os = "linux")]
    PickFile {
        /// The dialog's title.
        title: String,
        /// Where it starts.
        directory: Option<PathBuf>,
        /// Where the choice goes; `None` when cancelled.
        reply: std::sync::mpsc::Sender<Option<PathBuf>>,
    },
}

/// The lock held by this process once [`activate_existing`] found no other
/// app; [`run`] serves activations with it and releases it when it returns.
static PRIMARY: Mutex<Option<instance::Instance>> = Mutex::new(None);

/// Keeps to one tray app per user and config file. When another app runs
/// with `config_path`, asks it to show its settings window and returns
/// `true`: this process should exit. Otherwise takes the lock, so later
/// starts reach this one once [`run`] runs, and returns `false`.
pub fn activate_existing(config_path: &Path) -> Result<bool, DesktopError> {
    let mut primary = PRIMARY.lock().unwrap_or_else(PoisonError::into_inner);
    if primary.is_some() {
        return Ok(false);
    }
    let dir = anya_config::runtime_dir()?;
    match instance::claim(&dir, &instance::key(config_path)) {
        Ok(instance::Claim::Primary(instance)) => {
            *primary = Some(instance);
            Ok(false)
        }
        Ok(instance::Claim::Activated) => Ok(true),
        Err(e) => Err(DesktopError::Instance {
            path: e.path,
            source: e.source,
        }),
    }
}

/// Runs the tray app until `quit` becomes true or the user quits (which
/// calls [`Control::quit`]). Must run on the main thread; serves the page
/// on `runtime`. [`DesktopError::NoWindow`] means nothing could be shown and
/// the caller should fall back to the terminal UI. Releases the lock that
/// [`activate_existing`] took when it returns.
pub fn run(
    control: Arc<dyn Control>,
    catalog: Catalog,
    runtime: tokio::runtime::Handle,
    quit: tokio::sync::watch::Receiver<bool>,
) -> Result<(), DesktopError> {
    let instance = PRIMARY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    app::run(control, catalog, runtime, quit, instance)
}
