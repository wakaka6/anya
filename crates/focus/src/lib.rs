//! Which app has the keyboard focus: a macOS bundle id or an X11 window
//! class, for the context hooks of docs/decisions/0016-dictation-hooks.md.

use anya_core::desktop::Desktop;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod x11;

/// The app that has the keyboard focus right now, or `None` when it cannot
/// be told.
///
/// * macOS: the bundle id of the frontmost application, such as
///   `com.apple.Terminal`.
/// * X11: the class (the second string of `WM_CLASS`, the first when there is
///   no second) of the window named by the root window's
///   `_NET_ACTIVE_WINDOW`.
/// * Wayland: always `None`; the protocol has no way to ask which client has
///   the focus.
///
/// Blocking, and a few milliseconds at most. Failures are logged at debug
/// level and come back as `None`.
pub fn focused_app(desktop: Desktop) -> Option<String> {
    match desktop {
        #[cfg(target_os = "macos")]
        Desktop::Darwin => darwin::focused_app(),
        #[cfg(target_os = "linux")]
        Desktop::X11 => match x11::focused_app() {
            Ok(app) => app,
            Err(error) => {
                tracing::debug!(%error, "focused app unavailable");
                None
            }
        },
        Desktop::Wayland => None,
        #[allow(unreachable_patterns)] // a desktop this build has no backend for
        _ => None,
    }
}
