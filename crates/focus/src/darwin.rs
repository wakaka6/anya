//! The macOS backend: the frontmost application's bundle id.

use objc2_app_kit::NSWorkspace;

/// The bundle id of the frontmost application; `None` for a process without
/// one.
pub(crate) fn focused_app() -> Option<String> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication();
    let id = app
        .and_then(|app| app.bundleIdentifier())
        .map(|id| id.to_string());
    if id.is_none() {
        tracing::debug!("frontmost application has no bundle id");
    }
    id
}
