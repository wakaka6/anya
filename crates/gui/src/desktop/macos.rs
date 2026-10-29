//! What tao does not expose on macOS.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};
use tao::platform::macos::WindowExtMacOS;
use tao::window::Window;

/// Whether this is the process's main thread, the only one AppKit runs on.
pub(crate) fn on_main_thread() -> bool {
    MainThreadMarker::new().is_some()
}

/// Lets the panel show on every space and over full-screen apps, and keeps
/// it out of the window cycle (Cmd+`).
pub(crate) fn float_over_spaces(window: &Window) {
    let ptr = window.ns_window().cast::<NSWindow>();
    // SAFETY: `ns_window` is the live NSWindow that `window` owns; it stays
    // valid while `window` is borrowed, and this runs on the main thread
    // (the event loop's), where AppKit objects may be touched.
    let ns_window = unsafe { &*ptr };
    ns_window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
}
