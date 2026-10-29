//! The settings page and the tray panel, served over loopback HTTP to the
//! desktop window (tao, wry, tray-icon) and, with `--web`, to a browser.
//!
//! [`web`] serves the page and its JSON API for any [`Shell`]; [`desktop`]
//! is the tray app, a [`Shell`] that opens the page in its own windows.

use std::path::{Path, PathBuf};

pub mod desktop;
pub mod web;

/// Which shell shows the page; the page hides what a shell cannot do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    /// The desktop window and tray panel.
    App,
    /// A browser tab (`--web`).
    Web,
}

/// What only the shell around the page can do. Errors are untranslated.
pub trait Shell: Send + Sync {
    /// Which shell this is.
    fn kind(&self) -> ShellKind;

    /// Opens `url` in the system browser.
    fn open_url(&self, url: &str) -> Result<(), String>;

    /// Asks the user for a file, starting at `start`; `Ok(None)` when cancelled.
    fn pick_file(&self, start: Option<&Path>) -> Result<Option<PathBuf>, String>;

    /// Shows, unhides and focuses the settings window, hiding the tray panel.
    fn show_settings(&self);

    /// Ends Anya.
    fn quit(&self);
}
