//! The recording overlay: the notch on macOS, a layer-shell surface on
//! Wayland, a capsule window on X11.
//!
//! The overlay draws what it is told. The app decides the [`View`] from the
//! dictation trigger's state and the frames it reads, and passes every string
//! already translated: this crate holds no user text.

use std::fmt;
use std::str::FromStr;

use anya_core::desktop::Desktop;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
use darwin as platform;
#[cfg(target_os = "linux")]
use linux as platform;

/// The argument the binary starts itself with to run the macOS overlay
/// helper; it then calls [`helper_main`] and nothing else.
pub const HELPER_ARG: &str = "__anya-overlay-helper";

/// Runs the macOS overlay helper process: AppKit on its main thread, views
/// read from stdin. Never returns.
#[cfg(target_os = "macos")]
pub fn helper_main() -> ! {
    darwin::helper_main()
}

/// What the overlay needs from the host on `desktop`.
pub fn checks(desktop: Desktop) -> Vec<anya_core::doctor::Check> {
    platform::checks(desktop)
}

/// Where the overlay sits on the screen under the mouse pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Position {
    /// Grows out of the notch; the top center of a screen without one.
    /// Linux reads it as [`Position::TopCenter`].
    Notch,
    /// Top left corner.
    TopLeft,
    /// Top center.
    TopCenter,
    /// Top right corner.
    TopRight,
    /// Bottom left corner.
    BottomLeft,
    /// Bottom center.
    BottomCenter,
    /// Bottom right corner.
    BottomRight,
}

impl Position {
    /// Every position, in the order settings UIs list them.
    pub const ALL: [Position; 7] = [
        Position::Notch,
        Position::TopLeft,
        Position::TopCenter,
        Position::TopRight,
        Position::BottomLeft,
        Position::BottomCenter,
        Position::BottomRight,
    ];

    /// The config value.
    pub fn as_str(self) -> &'static str {
        match self {
            Position::Notch => "notch",
            Position::TopLeft => "top-left",
            Position::TopCenter => "top-center",
            Position::TopRight => "top-right",
            Position::BottomLeft => "bottom-left",
            Position::BottomCenter => "bottom-center",
            Position::BottomRight => "bottom-right",
        }
    }

    /// The default: the notch on macOS, bottom center elsewhere.
    pub fn platform_default() -> Position {
        if cfg!(target_os = "macos") {
            Position::Notch
        } else {
            Position::BottomCenter
        }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A position the config names that does not exist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown overlay position {0:?}")]
pub struct UnknownPosition(pub String);

impl FromStr for Position {
    type Err = UnknownPosition;

    fn from_str(s: &str) -> Result<Position, UnknownPosition> {
        Position::ALL
            .into_iter()
            .find(|p| p.as_str() == s.trim())
            .ok_or_else(|| UnknownPosition(s.to_owned()))
    }
}

/// How the overlay looks; changes apply without a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// Where it sits.
    pub position: Position,
    /// Size factor, 1.0 normal.
    pub scale: f64,
    /// Shown while idle too, as a small marker.
    pub idle_visible: bool,
    /// Shows recognized text, live and final.
    pub show_text: bool,
}

/// Translated words the overlay draws on its own, outside any [`View`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Labels {
    /// While recording with no text yet.
    pub listening: String,
    /// After the key is released, while the final result is awaited.
    pub recognizing: String,
    /// The idle marker, when [`Style::idle_visible`].
    pub idle: String,
}

/// What the overlay shows now. Sending the same kind again updates it in
/// place (a new level, longer text) without restarting its animation.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// Nothing is happening; hidden unless [`Style::idle_visible`].
    Idle,
    /// Recording: the meter at `level` (0.0 to 1.0) and the text so far.
    Listening {
        /// The latest chunk's level.
        level: f32,
        /// The session's text so far; may be empty.
        text: String,
    },
    /// The key is released; waiting for the final result.
    Finishing {
        /// The session's text so far.
        text: String,
    },
    /// The session is over and its text went somewhere. Held briefly, then
    /// the overlay returns to [`View::Idle`] by itself.
    Done {
        /// The final text; empty shows `label` alone.
        text: String,
        /// Where it went: copied, pasted, transcribed.
        label: String,
    },
    /// The session failed. Stays until the next view.
    Error {
        /// What went wrong.
        message: String,
        /// What the user can do, such as retrying.
        hint: String,
    },
}

/// Why the overlay could not start.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    /// The display server or window system refused.
    #[error("overlay backend failed: {0}")]
    Backend(String),
}

/// A running overlay. Dropping it closes the window.
pub struct Overlay {
    backend: platform::Backend,
}

impl Overlay {
    /// Opens the overlay for `desktop`, showing [`View::Idle`].
    pub fn start(desktop: Desktop, style: Style, labels: Labels) -> Result<Overlay, OverlayError> {
        Ok(Overlay {
            backend: platform::Backend::start(desktop, style, labels)?,
        })
    }

    /// Shows `view`. Never blocks: views sent faster than the overlay draws
    /// are coalesced, the latest wins.
    pub fn show(&self, view: View) {
        self.backend.show(view);
    }

    /// Applies a new style, reopening the window when the position or scale
    /// needs it; the current view stays.
    pub fn restyle(&self, style: Style) {
        self.backend.restyle(style);
    }
}
