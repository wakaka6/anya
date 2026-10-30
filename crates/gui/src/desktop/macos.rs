//! What tao does not expose on macOS.

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowCollectionBehavior,
    NSWindowOrderingMode,
};
use tao::platform::macos::WindowExtMacOS;
use tao::window::Window;

/// Run by the settings page before its own scripts: tells it the window
/// has the sidebar material behind it, so the sidebar can let it through.
pub(crate) const VIBRANT_SCRIPT: &str = "(() => { const on = () => document.documentElement.classList.add('vibrant'); if (document.documentElement) on(); else document.addEventListener('DOMContentLoaded', on); })();";

/// The tray panel's corner radius, the same as the page draws its edge with.
const PANEL_RADIUS: f64 = 12.0;

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

/// What shows behind a window's transparent webview.
pub(crate) enum Backdrop {
    /// System Settings' sidebar, following the window's active state.
    Sidebar,
    /// A menu bar extra's: the menu material, always active, with the
    /// rounded corners of Control Center's panels.
    Menu,
}

/// Puts `backdrop` behind the (transparent) webview, filling the window and
/// following it as it resizes.
pub(crate) fn backdrop(window: &Window, backdrop: Backdrop) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let ptr = window.ns_window().cast::<NSWindow>();
    // SAFETY: as in `float_over_spaces`.
    let ns_window = unsafe { &*ptr };
    let Some(content) = ns_window.contentView() else {
        return;
    };
    let effect =
        NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), content.bounds());
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    match backdrop {
        Backdrop::Sidebar => {
            effect.setMaterial(NSVisualEffectMaterial::Sidebar);
            effect.setState(NSVisualEffectState::FollowsWindowActiveState);
        }
        Backdrop::Menu => {
            effect.setMaterial(NSVisualEffectMaterial::Menu);
            effect.setState(NSVisualEffectState::Active);
            effect.setWantsLayer(true);
            if let Some(layer) = effect.layer() {
                layer.setCornerRadius(PANEL_RADIUS);
                layer.setMasksToBounds(true);
            }
            ns_window.setHasShadow(true);
        }
    }
    effect.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, None);
}
