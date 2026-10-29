//! The macOS overlay. The app process keeps a [`Backend`]: it starts the
//! same executable with [`crate::HELPER_ARG`] and writes JSON lines to its
//! stdin. The helper runs AppKit on its main thread and draws either the
//! notch (grows out of the camera housing, or the top center of a screen
//! without one) or a capsule at one of the other positions.
//!
//! Splitting the overlay into a process keeps AppKit's main-thread rule out
//! of the app: the app's main thread belongs to whatever else it runs.

#![allow(unsafe_code)]

mod backend;
mod ca;
mod capsule;
mod geometry;
mod helper;
mod notch;
mod wire;

use anya_core::desktop::Desktop;
use anya_core::doctor::{Check, Severity};

pub(crate) use backend::Backend;
pub(crate) use helper::helper_main;

/// The overlay needs a window server session: an SSH login has none, and
/// the helper's AppKit would refuse to start.
pub(crate) fn checks(desktop: Desktop) -> Vec<Check> {
    if desktop != Desktop::Darwin {
        return Vec::new();
    }
    if objc2_core_graphics::CGSessionCopyCurrentDictionary().is_some() {
        vec![Check::passed(
            "overlay_window_server",
            Severity::Warning,
            "present",
        )]
    } else {
        vec![Check::failed(
            "overlay_window_server",
            Severity::Warning,
            "missing",
        )]
    }
}
