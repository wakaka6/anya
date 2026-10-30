//! The tray app as the page's [`Shell`]: the system opener, the native file
//! dialog, and requests to the event loop for the windows.

use std::path::{Path, PathBuf};
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

/// Where the picker starts for the value `start`: its directory when it
/// names a file, itself when it names a directory, nothing when it is empty.
pub(crate) fn picker_directory(start: Option<&Path>) -> Option<PathBuf> {
    let start = start?.to_str()?.trim();
    if start.is_empty() {
        return None;
    }
    let path = anya_config::expand_home(start);
    if path.is_dir() {
        Some(path)
    } else {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_owned)
    }
}

pub(crate) struct AppShell {
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    pick_title: Mutex<String>,
}

impl AppShell {
    pub(crate) fn new(proxy: EventLoopProxy<UserEvent>, pick_title: String) -> AppShell {
        AppShell {
            proxy: Mutex::new(proxy),
            pick_title: Mutex::new(pick_title),
        }
    }

    /// The file dialog's title, after the language changed.
    pub(crate) fn set_pick_title(&self, title: &str) {
        let mut current = self
            .pick_title
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        title.clone_into(&mut current);
    }

    /// Whether the event loop took `event`; it is gone once the app ends.
    pub(super) fn send(&self, event: UserEvent) -> bool {
        let proxy = self.proxy.lock().unwrap_or_else(PoisonError::into_inner);
        proxy.send_event(event).is_ok()
    }

    fn pick_title(&self) -> String {
        self.pick_title
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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

    #[cfg(target_os = "macos")]
    fn pick_file(&self, start: Option<&Path>) -> Result<Option<PathBuf>, String> {
        let dialog = rfd::FileDialog::new().set_title(self.pick_title());
        let dialog = match picker_directory(start) {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        };
        // Runs the panel modally on the main thread and waits for it here.
        Ok(dialog.pick_file())
    }

    #[cfg(target_os = "linux")]
    fn pick_file(&self, start: Option<&Path>) -> Result<Option<PathBuf>, String> {
        // GTK lives on the event loop's thread: the dialog opens there and
        // answers here.
        let (reply, answer) = std::sync::mpsc::channel();
        let asked = self.send(UserEvent::PickFile {
            title: self.pick_title(),
            directory: picker_directory(start),
            reply,
        });
        if !asked {
            return Err("the app is closing".to_owned());
        }
        answer
            .recv()
            .map_err(|_| "the file dialog closed without an answer".to_owned())
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

    #[test]
    fn the_picker_starts_beside_the_current_file() {
        let dir = std::env::temp_dir();
        let file = dir.join("hotwords.txt");
        assert_eq!(picker_directory(Some(&file)), Some(dir.clone()));
        assert_eq!(picker_directory(Some(&dir)), Some(dir));
        assert_eq!(picker_directory(Some(Path::new("  "))), None);
        assert_eq!(picker_directory(Some(Path::new("hotwords.txt"))), None);
        assert_eq!(picker_directory(None), None);
    }

    #[test]
    fn the_picker_expands_home() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        assert_eq!(
            picker_directory(Some(Path::new("~/Documents/hotwords.txt"))),
            Some(home.join("Documents"))
        );
    }
}
