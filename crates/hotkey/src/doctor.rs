//! What the hotkey backend needs from the machine.

use anya_core::desktop::Desktop;
use anya_core::doctor::{Check, Severity};

/// Probes what the hotkey backend for `desktop` needs:
///
/// - `accessibility` on macOS, outcomes `granted` / `denied`; a denial
///   asks macOS to show its Accessibility prompt;
/// - `input_devices` on Wayland (evdev), outcomes `readable` (value
///   `device`), `none`, and for an unreadable node (value `device`)
///   `not_in_group`, `group_not_refreshed`, `no_input_group` or `denied`
///   (the process is in `input` and still cannot read it);
/// - `x11_display` on X11, outcomes `present` (value `display`) /
///   `missing`;
/// - `hotkeys` with outcome `unsupported_desktop` (value `desktop`) where
///   this build has no backend.
///
/// Each is [`Severity::Required`]: without it no hotkey fires.
pub fn checks(desktop: Desktop) -> Vec<Check> {
    match desktop {
        #[cfg(target_os = "macos")]
        Desktop::Darwin => vec![crate::darwin::accessibility_check()],
        #[cfg(target_os = "linux")]
        Desktop::Wayland => vec![linux::input_devices()],
        #[cfg(target_os = "linux")]
        Desktop::X11 => vec![x11_display(std::env::var("DISPLAY").ok())],
        other => vec![
            Check::failed("hotkeys", Severity::Required, "unsupported_desktop")
                .with("desktop", other.name()),
        ],
    }
}

/// The `x11_display` check over the value of `DISPLAY`.
#[cfg(any(target_os = "linux", test))]
fn x11_display(display: Option<String>) -> Check {
    match display.filter(|d| !d.trim().is_empty()) {
        Some(d) => Check::passed("x11_display", Severity::Required, "present").with("display", d),
        None => Check::failed("x11_display", Severity::Required, "missing"),
    }
}

/// How the user relates to the `input` group.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Membership {
    /// The system has an `input` group.
    group_exists: bool,
    /// This process runs with it.
    in_process: bool,
    /// The user is a member in the group database.
    in_database: bool,
}

/// Why an event node is unreadable, from the user's group membership.
#[cfg(any(target_os = "linux", test))]
fn denied_outcome(m: Membership) -> &'static str {
    if !m.group_exists {
        "no_input_group"
    } else if m.in_process {
        "denied"
    } else if m.in_database {
        "group_not_refreshed"
    } else {
        "not_in_group"
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::path::PathBuf;

    use anya_core::doctor::{Check, Severity};
    use nix::unistd::{Group, User, getgid, getgroups, getuid};

    use super::{Membership, denied_outcome};

    const CODE: &str = "input_devices";

    pub(super) fn input_devices() -> Check {
        let mut nodes: Vec<PathBuf> = match std::fs::read_dir("/dev/input") {
            Ok(dir) => dir
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("event"))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        nodes.sort();
        let mut denied = None;
        for node in &nodes {
            // Opened only for the probe: the file closes right away.
            match std::fs::File::open(node) {
                Ok(_) => {
                    return Check::passed(CODE, Severity::Required, "readable")
                        .with("device", node.display().to_string());
                }
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    denied.get_or_insert(node);
                }
                Err(_) => {}
            }
        }
        let Some(node) = denied else {
            return Check::failed(CODE, Severity::Required, "none");
        };
        Check::failed(CODE, Severity::Required, denied_outcome(membership()))
            .with("device", node.display().to_string())
    }

    fn membership() -> Membership {
        let Ok(Some(group)) = Group::from_name("input") else {
            return Membership {
                group_exists: false,
                in_process: false,
                in_database: false,
            };
        };
        let in_process =
            getgid() == group.gid || getgroups().is_ok_and(|ids| ids.contains(&group.gid));
        let in_database = User::from_uid(getuid())
            .ok()
            .flatten()
            .is_some_and(|u| u.gid == group.gid || group.mem.contains(&u.name));
        Membership {
            group_exists: true,
            in_process,
            in_database,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_membership_decides_why_input_is_unreadable() {
        let m = |group_exists, in_process, in_database| Membership {
            group_exists,
            in_process,
            in_database,
        };
        assert_eq!(denied_outcome(m(false, false, false)), "no_input_group");
        assert_eq!(denied_outcome(m(true, false, false)), "not_in_group");
        assert_eq!(denied_outcome(m(true, false, true)), "group_not_refreshed");
        assert_eq!(denied_outcome(m(true, true, true)), "denied");
    }

    #[test]
    fn the_x11_check_needs_a_display() {
        let ok = x11_display(Some(":0".into()));
        assert!(ok.ok);
        assert_eq!(ok.outcome, "present");
        assert_eq!(ok.values, [("display", ":0".to_owned())]);
        for missing in [None, Some(String::new()), Some(" ".into())] {
            let c = x11_display(missing);
            assert!(c.blocks());
            assert_eq!(c.outcome, "missing");
        }
    }
}
