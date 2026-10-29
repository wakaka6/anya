//! Paste into the focused field and write the clipboard.
//!
//! [`deliver`] puts text on the clipboard and, for [`Delivery::Paste`],
//! presses the paste keys in the focused app:
//!
//! - macOS: the general pasteboard, then Cmd+V posted with CGEvent. Needs the
//!   Accessibility permission.
//! - X11: `xclip` for CLIPBOARD and PRIMARY, then Shift+Insert through XTest;
//!   the previous CLIPBOARD comes back afterwards.
//! - Wayland: `wl-copy`, then Shift+Insert through `wtype`, or a virtual
//!   keyboard on `/dev/uinput` where `wtype` is missing, fails, or the
//!   compositor is KWin.
//!
//! Shift+Insert rather than Ctrl+V on Linux: it pastes in terminals as well as
//! in other apps.

use std::fmt;

use anya_core::desktop::Desktop;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // AppKit's pasteboard type constant is an extern static.
mod darwin;
mod doctor;
#[cfg(target_os = "linux")]
mod linux;
mod plan;

pub use doctor::checks;

/// Where delivered text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Delivery {
    /// Into the focused field, through the clipboard and the paste keys.
    Paste,
    /// Onto the clipboard only.
    Clipboard,
}

/// What a delivery was doing when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Reading the clipboard.
    ReadClipboard,
    /// Writing the clipboard.
    SetClipboard,
    /// Writing the primary selection.
    SetPrimary,
    /// Pressing the paste keys.
    Paste,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Action::ReadClipboard => "read clipboard",
            Action::SetClipboard => "set clipboard",
            Action::SetPrimary => "set primary selection",
            Action::Paste => "simulate paste",
        })
    }
}

/// A permission a paste needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// macOS Accessibility, to post key events.
    Accessibility,
    /// Write access to `/dev/uinput`, to create a virtual keyboard.
    Uinput,
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Permission::Accessibility => "macOS Accessibility permission",
            Permission::Uinput => "write access to /dev/uinput",
        })
    }
}

/// Why a delivery failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    /// An external tool the desktop needs is not on PATH.
    #[error("{tool} is not installed")]
    MissingTool {
        /// The executable's name, as [`missing_tools`] reports it.
        tool: &'static str,
    },
    /// The system refused a permission the paste needs.
    #[error("{what} is missing: {detail}")]
    Permission {
        /// Which permission.
        what: Permission,
        /// The system's words.
        detail: String,
    },
    /// The clipboard or the key events failed otherwise.
    #[error("{action}: {detail}")]
    Backend {
        /// The step that failed.
        action: Action,
        /// The tool's or the system's words.
        detail: String,
    },
    /// This build has no backend for the desktop (a Linux desktop on macOS or
    /// the other way round).
    #[error("this build cannot deliver text on {0}")]
    Unsupported(Desktop),
}

/// Delivers `text` on `desktop`, verbatim. Blocks for up to a few hundred
/// milliseconds plus the tools' run time (each is stopped after 3 s); run it
/// on a blocking task.
///
/// When pasting fails after the clipboard was written, the clipboard keeps
/// the text, so the user can still paste it by hand.
pub fn deliver(desktop: Desktop, text: &str, how: Delivery) -> Result<(), OutputError> {
    tracing::debug!(%desktop, ?how, len = text.len(), "delivering text");
    match desktop {
        #[cfg(target_os = "macos")]
        Desktop::Darwin => {
            let steps = plan::plan(desktop, how, plan::Session::default());
            plan::execute(&mut darwin::Pasteboard, &steps, text)
        }
        #[cfg(target_os = "linux")]
        Desktop::X11 | Desktop::Wayland => {
            let session = plan::Session::detect();
            let steps = plan::plan(desktop, how, session);
            plan::execute(&mut linux::Tools::new(desktop), &steps, text)
        }
        other => Err(OutputError::Unsupported(other)),
    }
}

/// The external tools `desktop` needs that are not on PATH: `xclip` on X11,
/// `wl-copy` on Wayland, none on macOS. `wtype` is optional on Wayland
/// (`/dev/uinput` stands in), so it is never listed.
pub fn missing_tools(desktop: Desktop) -> Vec<&'static str> {
    plan::missing(desktop, |tool| plan::which(tool).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_name_the_tool_permission_or_step() {
        assert_eq!(
            OutputError::MissingTool { tool: "xclip" }.to_string(),
            "xclip is not installed"
        );
        assert_eq!(
            OutputError::Permission {
                what: Permission::Uinput,
                detail: "Permission denied (os error 13)".into()
            }
            .to_string(),
            "write access to /dev/uinput is missing: Permission denied (os error 13)"
        );
        assert_eq!(
            OutputError::Backend {
                action: Action::SetClipboard,
                detail: "exit status 1".into()
            }
            .to_string(),
            "set clipboard: exit status 1"
        );
    }

    #[test]
    fn a_desktop_of_the_other_os_is_unsupported() {
        let foreign = if cfg!(target_os = "macos") {
            Desktop::X11
        } else {
            Desktop::Darwin
        };
        assert_eq!(
            deliver(foreign, "x", Delivery::Clipboard),
            Err(OutputError::Unsupported(foreign))
        );
    }
}
