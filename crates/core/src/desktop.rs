//! Which desktop Anya runs on. Adapters pick their backend by it; the app
//! decides it once, from `--backend` or the environment.

use std::fmt;
use std::str::FromStr;

/// A desktop Anya supports (docs/decisions/0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Desktop {
    /// macOS.
    Darwin,
    /// A Linux Wayland session.
    Wayland,
    /// A Linux X11 session.
    X11,
}

impl Desktop {
    /// The desktop of this session: macOS on macOS; on Linux Wayland when
    /// `WAYLAND_DISPLAY` is set or `XDG_SESSION_TYPE` says so, otherwise X11.
    pub fn detect() -> Desktop {
        Desktop::from_env(cfg!(target_os = "macos"), |k| std::env::var(k).ok())
    }

    /// [`Desktop::detect`] over a given environment.
    pub fn from_env(macos: bool, var: impl Fn(&str) -> Option<String>) -> Desktop {
        if macos {
            return Desktop::Darwin;
        }
        let set = |k: &str| var(k).is_some_and(|v| !v.trim().is_empty());
        let session = var("XDG_SESSION_TYPE")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if set("WAYLAND_DISPLAY") || session == "wayland" {
            Desktop::Wayland
        } else {
            Desktop::X11
        }
    }

    /// The name `--backend` and logs use.
    pub fn name(self) -> &'static str {
        match self {
            Desktop::Darwin => "darwin",
            Desktop::Wayland => "wayland",
            Desktop::X11 => "x11",
        }
    }
}

impl fmt::Display for Desktop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A `--backend` value that names no desktop.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown backend {0:?}; expected darwin, wayland or x11")]
pub struct UnknownDesktop(pub String);

impl FromStr for Desktop {
    type Err = UnknownDesktop;

    fn from_str(s: &str) -> Result<Desktop, UnknownDesktop> {
        match s.trim().to_ascii_lowercase().as_str() {
            "darwin" | "macos" => Ok(Desktop::Darwin),
            "wayland" => Ok(Desktop::Wayland),
            "x11" => Ok(Desktop::X11),
            _ => Err(UnknownDesktop(s.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_type_decides_on_linux() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            Desktop::from_env(true, env(&[("WAYLAND_DISPLAY", "w")])),
            Desktop::Darwin
        );
        assert_eq!(
            Desktop::from_env(false, env(&[("WAYLAND_DISPLAY", "wayland-0")])),
            Desktop::Wayland
        );
        assert_eq!(
            Desktop::from_env(false, env(&[("XDG_SESSION_TYPE", "Wayland")])),
            Desktop::Wayland
        );
        assert_eq!(
            Desktop::from_env(false, env(&[("WAYLAND_DISPLAY", " ")])),
            Desktop::X11
        );
        assert_eq!(Desktop::from_env(false, env(&[])), Desktop::X11);
        assert_eq!("X11".parse(), Ok(Desktop::X11));
        assert!("windows".parse::<Desktop>().is_err());
    }
}
