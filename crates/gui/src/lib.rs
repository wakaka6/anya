//! The settings page and the tray panel, served over loopback HTTP to the
//! desktop window (tao, wry, tray-icon) and, with `--web`, to a browser.
//!
//! [`web`] serves the page and its JSON API for any [`Shell`]; [`desktop`]
//! is the tray app, a [`Shell`] that opens the page in its own windows.

pub mod desktop;
pub mod web;

/// The macOS pane that grants Accessibility, which global hotkeys need: the
/// one link besides web pages the shell opens, and only on the page's asking.
pub(crate) const PRIVACY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

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

    /// Shows, unhides and focuses the settings window, hiding the tray panel.
    fn show_settings(&self);

    /// Ends Anya.
    fn quit(&self);
}
