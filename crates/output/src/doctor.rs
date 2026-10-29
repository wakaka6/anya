//! What delivering text needs from the host, for the doctor.
//!
//! - `clipboard_tool` (required): the clipboard tool of a Linux desktop,
//!   `found` or `missing`.
//! - `paste_keys` (warning; copying still works without it): a way to press
//!   the paste keys. macOS: `granted` or `not_granted`. Wayland: `wtype`,
//!   `uinput`, or `denied` / `kde_denied` (KWin cannot use `wtype`, so only
//!   `/dev/uinput` helps there). X11 needs nothing beyond the X server.

use anya_core::desktop::Desktop;
use anya_core::doctor::{Check, Severity};

use crate::plan;

/// The host checks delivery needs on `desktop`: each clipboard tool, and the
/// way to press the paste keys. Cheap and non-blocking; on macOS a missing
/// Accessibility permission makes the system show its dialog. A desktop this
/// build has no backend for gets no checks.
pub fn checks(desktop: Desktop) -> Vec<Check> {
    if cfg!(target_os = "macos") != (desktop == Desktop::Darwin) {
        return Vec::new();
    }
    let mut checks: Vec<Check> = plan::required_tools(desktop)
        .iter()
        .map(|tool| clipboard_tool(tool, plan::which(tool)))
        .collect();
    checks.extend(paste_keys(desktop));
    checks
}

/// The package that installs `tool`.
fn package(tool: &str) -> &str {
    match tool {
        "wl-copy" | "wl-paste" => "wl-clipboard",
        other => other,
    }
}

fn clipboard_tool(tool: &'static str, found: Option<std::path::PathBuf>) -> Check {
    let check = match found {
        Some(path) => Check::passed("clipboard_tool", Severity::Required, "found")
            .with("path", path.display().to_string()),
        None => Check::failed("clipboard_tool", Severity::Required, "missing"),
    };
    check.with("tool", tool).with("package", package(tool))
}

#[cfg(target_os = "macos")]
fn paste_keys(desktop: Desktop) -> Option<Check> {
    (desktop == Desktop::Darwin).then(|| macos(crate::darwin::may_post_events()))
}

#[cfg(target_os = "linux")]
fn paste_keys(desktop: Desktop) -> Option<Check> {
    (desktop == Desktop::Wayland).then(|| {
        let uinput = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map(drop);
        wayland(
            plan::is_kde(|k| std::env::var(k).ok()),
            plan::which("wtype"),
            uinput,
        )
    })
}

#[cfg(any(target_os = "macos", test))]
fn macos(granted: bool) -> Check {
    if granted {
        Check::passed("paste_keys", Severity::Warning, "granted")
    } else {
        Check::failed("paste_keys", Severity::Warning, "not_granted")
    }
}

/// The Wayland paste check from what the probes found: `wtype` (outside
/// KDE) or a writable `/dev/uinput`.
#[cfg(any(target_os = "linux", test))]
fn wayland(kde: bool, wtype: Option<std::path::PathBuf>, uinput: std::io::Result<()>) -> Check {
    const CODE: &str = "paste_keys";
    match (wtype, uinput) {
        (Some(path), _) if !kde => {
            Check::passed(CODE, Severity::Warning, "wtype").with("path", path.display().to_string())
        }
        (_, Ok(())) => Check::passed(CODE, Severity::Warning, "uinput"),
        (_, Err(e)) => Check::failed(
            CODE,
            Severity::Warning,
            if kde { "kde_denied" } else { "denied" },
        )
        .with("error", e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use super::*;

    fn denied() -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }

    #[test]
    fn a_clipboard_tool_is_required_and_names_its_package() {
        let found = clipboard_tool("xclip", Some(PathBuf::from("/usr/bin/xclip")));
        assert!(found.ok);
        assert_eq!(found.outcome, "found");
        assert_eq!(
            found.values,
            [
                ("path", "/usr/bin/xclip".to_owned()),
                ("tool", "xclip".to_owned()),
                ("package", "xclip".to_owned())
            ]
        );
        let missing = clipboard_tool("wl-copy", None);
        assert!(missing.blocks());
        assert_eq!(missing.outcome, "missing");
        assert_eq!(
            missing.values,
            [
                ("tool", "wl-copy".to_owned()),
                ("package", "wl-clipboard".to_owned())
            ]
        );
    }

    #[test]
    fn wayland_paste_needs_wtype_or_uinput() {
        let wtype = || Some(PathBuf::from("/usr/bin/wtype"));
        let c = wayland(false, wtype(), denied());
        assert_eq!((c.ok, c.outcome), (true, "wtype"));
        assert_eq!(c.values, [("path", "/usr/bin/wtype".to_owned())]);
        let c = wayland(false, None, Ok(()));
        assert_eq!((c.ok, c.outcome), (true, "uinput"));
        let c = wayland(false, None, denied());
        assert_eq!((c.ok, c.outcome), (false, "denied"));
        assert_eq!(c.values.first().map(|v| v.0), Some("error"));
        assert!(!c.blocks(), "copying still works without paste keys");
    }

    #[test]
    fn kde_needs_uinput_even_with_wtype() {
        let wtype = || Some(PathBuf::from("/usr/bin/wtype"));
        let c = wayland(true, wtype(), Ok(()));
        assert_eq!((c.ok, c.outcome), (true, "uinput"));
        let c = wayland(true, wtype(), denied());
        assert_eq!((c.ok, c.outcome), (false, "kde_denied"));
    }

    #[test]
    fn macos_reports_the_permission() {
        assert_eq!(macos(true).outcome, "granted");
        let c = macos(false);
        assert_eq!((c.ok, c.outcome), (false, "not_granted"));
        assert_eq!(c.severity, Severity::Warning);
    }

    #[test]
    fn x11_and_foreign_desktops_check_only_their_tools() {
        let codes = |d| checks(d).iter().map(|c| c.code).collect::<Vec<_>>();
        if cfg!(target_os = "macos") {
            assert!(codes(Desktop::X11).is_empty());
            assert!(codes(Desktop::Wayland).is_empty());
        } else {
            assert_eq!(codes(Desktop::X11), ["clipboard_tool"]);
            assert!(codes(Desktop::Darwin).is_empty());
        }
    }
}
