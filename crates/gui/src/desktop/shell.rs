//! The tray app as the page's [`Shell`]: the system opener and requests to
//! the event loop for the windows.

use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use tao::event_loop::EventLoopProxy;

use super::UserEvent;
use crate::{Shell, ShellKind};

/// Whether `url` may go to the system opener: a web page or the privacy
/// settings pane, nothing that runs a program or opens a local file.
pub(crate) fn openable(url: &str) -> bool {
    if url == crate::PRIVACY_SETTINGS_URL {
        return true;
    }
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && !url.chars().any(|c| c.is_whitespace() || c.is_control())
    })
}

/// Whether the webviews may navigate to `url`: the page's own server at
/// `origin` (`http://127.0.0.1:<port>`). Anything else opens in the browser.
pub(crate) fn same_origin(url: &str, origin: &str) -> bool {
    url.strip_prefix(origin)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
}

pub(crate) struct AppShell {
    proxy: Mutex<EventLoopProxy<UserEvent>>,
}

impl AppShell {
    pub(crate) fn new(proxy: EventLoopProxy<UserEvent>) -> AppShell {
        AppShell {
            proxy: Mutex::new(proxy),
        }
    }

    /// Whether the event loop took `event`; it is gone once the app ends.
    pub(super) fn send(&self, event: UserEvent) -> bool {
        let proxy = self.proxy.lock().unwrap_or_else(PoisonError::into_inner);
        proxy.send_event(event).is_ok()
    }
}

#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPENER: &str = "xdg-open";

impl Shell for AppShell {
    fn kind(&self) -> ShellKind {
        ShellKind::App
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        if !openable(url) {
            return Err(format!("not a web address: {url}"));
        }
        let mut child = Command::new(OPENER)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{OPENER}: {e}"))?;
        // Some openers stay until the browser exits; reap it off this thread.
        std::thread::spawn(move || match child.wait() {
            Ok(status) if !status.success() => {
                tracing::warn!(opener = OPENER, %status, "opening a link failed");
            }
            Err(e) => tracing::warn!(opener = OPENER, error = %e, "opening a link failed"),
            Ok(_) => {}
        });
        Ok(())
    }

    fn show_settings(&self) {
        // A closed loop means the app is ending; nothing is left to show.
        self.send(UserEvent::ShowSettings);
    }

    fn quit(&self) {
        self.send(UserEvent::Quit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pages_go_to_the_opener() {
        for ok in [
            "https://www.volcengine.com/docs/6561/1395867",
            "http://127.0.0.1:7590/",
            "HTTPS://platform.openai.com/docs",
            crate::PRIVACY_SETTINGS_URL,
        ] {
            assert!(openable(ok), "{ok}");
        }
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "x-apple.systempreferences:com.apple.preference.security",
            "/Applications/Calculator.app",
            "-a Calculator",
            "https://",
            "https:///etc/passwd",
            "https://example.com/a b",
            "https://example.com/\n--args",
            "",
        ] {
            assert!(!openable(refused), "{refused}");
        }
    }

    #[test]
    fn webviews_stay_on_the_page_server() {
        let origin = "http://127.0.0.1:4100";
        for ok in [
            "http://127.0.0.1:4100",
            "http://127.0.0.1:4100/",
            "http://127.0.0.1:4100/panel.html?key=ab",
            "http://127.0.0.1:4100#engines",
        ] {
            assert!(same_origin(ok, origin), "{ok}");
        }
        for refused in [
            "http://127.0.0.1:41000/",
            "http://127.0.0.1:4100.evil.example/",
            "http://127.0.0.1:4100@evil.example/",
            "https://127.0.0.1:4100/",
            "https://www.volcengine.com/docs/6561/1395867",
        ] {
            assert!(!same_origin(refused, origin), "{refused}");
        }
    }
}
