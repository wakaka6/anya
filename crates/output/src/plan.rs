//! What a delivery does, as data: the steps per desktop and mode, the key
//! sequences with their timings, and the order the steps run in. The platform
//! backends only carry out single steps, so everything here is testable
//! without a display.

use std::time::Duration;

use anya_core::desktop::Desktop;

use crate::{Delivery, OutputError};

/// Time between writing the clipboard and pressing the paste keys, so the
/// clipboard owner is serving before the focused app asks it.
pub(crate) const SETTLE: Duration = Duration::from_millis(50);
/// Time the X11 primary selection stays owned after the paste keys, so a slow
/// app still finds the text when it reads the selection.
pub(crate) const PRIMARY_HOLD: Duration = Duration::from_millis(300);

/// A key a paste presses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    /// Command on macOS.
    Command,
    /// The key at the ANSI `V` position on macOS.
    V,
    /// Left Shift on Linux.
    Shift,
    /// Insert on Linux.
    Insert,
}

/// One event of a key sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stroke {
    Down(Key),
    Up(Key),
    Wait(Duration),
}

const fn ms(n: u64) -> Stroke {
    Stroke::Wait(Duration::from_millis(n))
}

/// Cmd+V through CGEvent.
const CMD_V: &[Stroke] = &[
    Stroke::Down(Key::Command),
    ms(15),
    Stroke::Down(Key::V),
    ms(30),
    Stroke::Up(Key::V),
    ms(15),
    Stroke::Up(Key::Command),
];

/// Shift+Insert through XTest.
const SHIFT_INSERT_XTEST: &[Stroke] = &[
    Stroke::Down(Key::Shift),
    ms(15),
    Stroke::Down(Key::Insert),
    ms(30),
    Stroke::Up(Key::Insert),
    ms(15),
    Stroke::Up(Key::Shift),
];

/// Shift+Insert on a fresh uinput keyboard. The first wait lets the
/// compositor pick the new device up before it sends anything.
const SHIFT_INSERT_UINPUT: &[Stroke] = &[
    ms(80),
    Stroke::Down(Key::Shift),
    ms(15),
    Stroke::Down(Key::Insert),
    ms(15),
    Stroke::Up(Key::Insert),
    ms(15),
    Stroke::Up(Key::Shift),
];

/// How the paste keys are pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    /// Cmd+V posted with CGEvent (macOS).
    CgEvent,
    /// Shift+Insert faked through the XTest extension (X11).
    XTest,
    /// Shift+Insert typed by `wtype` (Wayland compositors with the virtual
    /// keyboard protocol).
    Wtype,
    /// Shift+Insert from a virtual keyboard on `/dev/uinput` (any Wayland
    /// compositor, given write access).
    Uinput,
}

impl Method {
    /// The key sequence this method plays itself; `wtype` gets its keys as
    /// arguments ([`WTYPE`]) instead.
    pub(crate) fn strokes(self) -> Option<&'static [Stroke]> {
        match self {
            Method::CgEvent => Some(CMD_V),
            Method::XTest => Some(SHIFT_INSERT_XTEST),
            Method::Wtype => None,
            Method::Uinput => Some(SHIFT_INSERT_UINPUT),
        }
    }

    /// The name logs use.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Method::CgEvent => "darwin/CGEventPost+Cmd+V",
            Method::XTest => "x11/XTest+Shift+Insert",
            Method::Wtype => "wayland/wtype+Shift+Insert",
            Method::Uinput => "wayland/uinput+Shift+Insert",
        }
    }
}

/// Plays `strokes`, sleeping on each wait and handing every key edge to
/// `send` (`true` for down).
pub(crate) fn play<E>(
    strokes: &[Stroke],
    mut send: impl FnMut(Key, bool) -> Result<(), E>,
) -> Result<(), E> {
    for stroke in strokes {
        match *stroke {
            Stroke::Down(key) => send(key, true)?,
            Stroke::Up(key) => send(key, false)?,
            Stroke::Wait(d) => std::thread::sleep(d),
        }
    }
    Ok(())
}

/// Commands the Linux backends run, text arriving on stdin.
#[cfg(target_os = "linux")]
pub(crate) mod cmd {
    /// Reads the X11 CLIPBOARD.
    pub(crate) const XCLIP_READ: &[&str] = &["xclip", "-o", "-selection", "clipboard"];
    /// Writes the X11 CLIPBOARD; xclip keeps serving it in the background.
    pub(crate) const XCLIP_WRITE: &[&str] = &["xclip", "-selection", "clipboard"];
    /// Owns the X11 PRIMARY selection in the foreground, so killing the
    /// process gives the selection up.
    pub(crate) const XCLIP_PRIMARY: &[&str] = &["xclip", "-quiet", "-selection", "primary"];
    /// Writes the Wayland clipboard.
    pub(crate) const WL_COPY: &[&str] = &["wl-copy", "--type", "text/plain;charset=utf-8"];
    /// Reads the Wayland clipboard as it was written.
    pub(crate) const WL_PASTE: &[&str] = &["wl-paste", "--no-newline"];
    /// Writes the Wayland primary selection.
    pub(crate) const WL_COPY_PRIMARY: &[&str] =
        &["wl-copy", "--primary", "--type", "text/plain;charset=utf-8"];
    /// Types Shift+Insert.
    pub(crate) const WTYPE: &[&str] = &["wtype", "-M", "shift", "-k", "Insert", "-m", "shift"];
}

/// What the environment adds to the choice of steps on Wayland.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Session {
    /// The session is KDE Plasma: KWin offers no virtual keyboard protocol
    /// to `wtype`, and writing the primary selection there makes the paste
    /// land twice.
    pub(crate) kde: bool,
    /// `wtype` is on PATH.
    pub(crate) wtype: bool,
}

impl Session {
    /// The session of this process.
    #[cfg(target_os = "linux")]
    pub(crate) fn detect() -> Session {
        let var = |k: &str| std::env::var(k).ok();
        Session {
            kde: is_kde(var),
            wtype: which("wtype").is_some(),
        }
    }
}

/// KDE Plasma: `XDG_CURRENT_DESKTOP` or `DESKTOP_SESSION` names KDE or
/// Plasma, or `KDE_SESSION_VERSION` is set.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn is_kde(var: impl Fn(&str) -> Option<String>) -> bool {
    let names = format!(
        "{} {}",
        var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        var("DESKTOP_SESSION").unwrap_or_default()
    )
    .to_lowercase();
    names.contains("kde")
        || names.contains("plasma")
        || var("KDE_SESSION_VERSION").is_some_and(|v| !v.is_empty())
}

/// The first executable file named `tool` in a `PATH` directory.
pub(crate) fn which(tool: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|file| is_executable(file))
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The external tools a desktop cannot deliver without. `wtype` is not one:
/// Wayland falls back to `/dev/uinput` without it.
pub(crate) fn required_tools(desktop: Desktop) -> &'static [&'static str] {
    match desktop {
        Desktop::Darwin => &[],
        Desktop::Wayland => &["wl-copy"],
        Desktop::X11 => &["xclip"],
    }
}

/// [`required_tools`] that `found` does not find.
pub(crate) fn missing(desktop: Desktop, found: impl Fn(&str) -> bool) -> Vec<&'static str> {
    required_tools(desktop)
        .iter()
        .copied()
        .filter(|tool| !found(tool))
        .collect()
}

/// One step of a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// Remember what the clipboard holds, to put it back afterwards.
    SaveClipboard,
    /// Put the text on the clipboard.
    SetClipboard,
    /// Put the text in the primary selection, until [`Step::ReleasePrimary`]
    /// or the end of the delivery.
    SetPrimary,
    Pause(Duration),
    /// Press the paste keys with the first method that works.
    Press(Vec<Method>),
    /// Give the primary selection up.
    ReleasePrimary,
    /// Put the remembered clipboard back if it held something else.
    RestoreClipboard,
}

/// The steps that deliver text on `desktop`.
///
/// - Clipboard: write the clipboard, nothing else, on every desktop.
/// - macOS paste: write the pasteboard, Cmd+V. The pasteboard keeps the text.
/// - X11 paste: remember the CLIPBOARD, write CLIPBOARD and PRIMARY (Shift+Insert
///   pastes PRIMARY in terminals and CLIPBOARD elsewhere), Shift+Insert through
///   XTest, give PRIMARY up after a while, put the old CLIPBOARD back.
/// - Wayland paste: write the clipboard and, outside KDE, the primary
///   selection; Shift+Insert through `wtype` when it is there and the
///   compositor is not KWin, else (or when `wtype` fails) through uinput. The
///   clipboard keeps the text.
pub(crate) fn plan(desktop: Desktop, how: Delivery, session: Session) -> Vec<Step> {
    use Step::*;
    if how == Delivery::Clipboard {
        return vec![SetClipboard];
    }
    match desktop {
        Desktop::Darwin => vec![SetClipboard, Pause(SETTLE), Press(vec![Method::CgEvent])],
        Desktop::X11 => vec![
            SaveClipboard,
            SetClipboard,
            SetPrimary,
            Pause(SETTLE),
            Press(vec![Method::XTest]),
            Pause(PRIMARY_HOLD),
            ReleasePrimary,
            RestoreClipboard,
        ],
        Desktop::Wayland => {
            let mut steps = vec![SetClipboard];
            if !session.kde {
                steps.push(SetPrimary);
            }
            steps.push(Pause(SETTLE));
            steps.push(Press(if session.wtype && !session.kde {
                vec![Method::Wtype, Method::Uinput]
            } else {
                vec![Method::Uinput]
            }));
            steps
        }
    }
}

/// What a platform does for each step.
pub(crate) trait Backend {
    fn read_clipboard(&mut self) -> Result<String, OutputError>;
    fn set_clipboard(&mut self, text: &str) -> Result<(), OutputError>;
    fn set_primary(&mut self, text: &str) -> Result<(), OutputError>;
    fn release_primary(&mut self);
    fn press(&mut self, method: Method) -> Result<(), OutputError>;
    fn pause(&mut self, d: Duration);
}

/// The clipboard text to put back: what was there, if anything, unless it is
/// the text just pasted.
pub(crate) fn to_restore<'a>(saved: Option<&'a str>, text: &str) -> Option<&'a str> {
    saved.filter(|s| !s.is_empty() && *s != text)
}

/// Runs `steps` for `text` on `backend`.
///
/// A failing step ends the delivery with its error; the primary selection is
/// given up, and the clipboard keeps the text so the user can still paste it
/// by hand. Reading or restoring the old clipboard never fails a delivery.
pub(crate) fn execute(
    backend: &mut dyn Backend,
    steps: &[Step],
    text: &str,
) -> Result<(), OutputError> {
    let mut saved = None;
    let mut holding = false;
    for step in steps {
        let done = match step {
            Step::SaveClipboard => {
                saved = backend
                    .read_clipboard()
                    .inspect_err(|e| tracing::debug!(error = %e, "clipboard not saved"))
                    .ok();
                Ok(())
            }
            Step::SetClipboard => backend.set_clipboard(text),
            Step::SetPrimary => {
                holding = true;
                backend.set_primary(text)
            }
            Step::Pause(d) => {
                backend.pause(*d);
                Ok(())
            }
            Step::Press(methods) => press(backend, methods),
            Step::ReleasePrimary => {
                holding = false;
                backend.release_primary();
                Ok(())
            }
            Step::RestoreClipboard => {
                if let Some(old) = to_restore(saved.as_deref(), text)
                    && let Err(e) = backend.set_clipboard(old)
                {
                    tracing::warn!(error = %e, "previous clipboard not restored");
                }
                Ok(())
            }
        };
        if let Err(e) = done {
            if holding {
                backend.release_primary();
            }
            return Err(e);
        }
    }
    Ok(())
}

fn press(backend: &mut dyn Backend, methods: &[Method]) -> Result<(), OutputError> {
    let mut last = None;
    for &method in methods {
        match backend.press(method) {
            Ok(()) => {
                tracing::debug!(method = method.label(), "paste keys sent");
                return Ok(());
            }
            Err(e) => {
                tracing::debug!(method = method.label(), error = %e, "paste keys failed");
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| OutputError::Backend {
        action: crate::Action::Paste,
        detail: "no way to press the paste keys".to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Action;
    use Step::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    const PLAIN: Session = Session {
        kde: false,
        wtype: true,
    };

    #[test]
    fn kde_is_found_by_desktop_name_or_session_version() {
        assert!(is_kde(env(&[("XDG_CURRENT_DESKTOP", "KDE")])));
        assert!(is_kde(env(&[("DESKTOP_SESSION", "plasmawayland")])));
        assert!(is_kde(env(&[("KDE_SESSION_VERSION", "6")])));
        assert!(!is_kde(env(&[("XDG_CURRENT_DESKTOP", "GNOME")])));
        assert!(!is_kde(env(&[
            ("XDG_CURRENT_DESKTOP", "sway"),
            ("KDE_SESSION_VERSION", "")
        ])));
        assert!(!is_kde(env(&[])));
    }

    #[test]
    fn clipboard_mode_only_writes_the_clipboard() {
        for desktop in [Desktop::Darwin, Desktop::X11, Desktop::Wayland] {
            for session in [
                PLAIN,
                Session {
                    kde: true,
                    wtype: false,
                },
            ] {
                assert_eq!(
                    plan(desktop, Delivery::Clipboard, session),
                    vec![SetClipboard]
                );
            }
        }
    }

    #[test]
    fn macos_pastes_with_cmd_v_and_keeps_the_text() {
        assert_eq!(
            plan(Desktop::Darwin, Delivery::Paste, PLAIN),
            vec![SetClipboard, Pause(SETTLE), Press(vec![Method::CgEvent])]
        );
    }

    #[test]
    fn x11_pastes_with_both_selections_then_restores() {
        assert_eq!(
            plan(Desktop::X11, Delivery::Paste, PLAIN),
            vec![
                SaveClipboard,
                SetClipboard,
                SetPrimary,
                Pause(SETTLE),
                Press(vec![Method::XTest]),
                Pause(PRIMARY_HOLD),
                ReleasePrimary,
                RestoreClipboard,
            ]
        );
    }

    #[test]
    fn wayland_prefers_wtype_and_falls_back_to_uinput() {
        assert_eq!(
            plan(Desktop::Wayland, Delivery::Paste, PLAIN),
            vec![
                SetClipboard,
                SetPrimary,
                Pause(SETTLE),
                Press(vec![Method::Wtype, Method::Uinput])
            ]
        );
    }

    #[test]
    fn wayland_without_wtype_uses_uinput() {
        let session = Session {
            kde: false,
            wtype: false,
        };
        assert_eq!(
            plan(Desktop::Wayland, Delivery::Paste, session),
            vec![
                SetClipboard,
                SetPrimary,
                Pause(SETTLE),
                Press(vec![Method::Uinput])
            ]
        );
    }

    #[test]
    fn kde_skips_wtype_and_the_primary_selection() {
        for wtype in [true, false] {
            let session = Session { kde: true, wtype };
            assert_eq!(
                plan(Desktop::Wayland, Delivery::Paste, session),
                vec![SetClipboard, Pause(SETTLE), Press(vec![Method::Uinput])]
            );
        }
    }

    #[test]
    fn uinput_waits_for_the_compositor_then_types_shift_insert() {
        use Key::*;
        let wait = |n| Stroke::Wait(Duration::from_millis(n));
        assert_eq!(
            Method::Uinput.strokes(),
            Some(
                &[
                    wait(80),
                    Stroke::Down(Shift),
                    wait(15),
                    Stroke::Down(Insert),
                    wait(15),
                    Stroke::Up(Insert),
                    wait(15),
                    Stroke::Up(Shift),
                ][..]
            )
        );
    }

    #[test]
    fn every_sequence_releases_what_it_pressed_in_reverse() {
        for method in [Method::CgEvent, Method::XTest, Method::Uinput] {
            let keys: Vec<_> = method
                .strokes()
                .unwrap_or_default()
                .iter()
                .filter(|s| !matches!(s, Stroke::Wait(_)))
                .copied()
                .collect();
            let (m, k) = match method {
                Method::CgEvent => (Key::Command, Key::V),
                _ => (Key::Shift, Key::Insert),
            };
            assert_eq!(
                keys,
                [
                    Stroke::Down(m),
                    Stroke::Down(k),
                    Stroke::Up(k),
                    Stroke::Up(m)
                ],
                "{method:?}"
            );
        }
        assert_eq!(Method::Wtype.strokes(), None);
        #[cfg(target_os = "linux")]
        assert_eq!(
            cmd::WTYPE,
            ["wtype", "-M", "shift", "-k", "Insert", "-m", "shift"]
        );
    }

    #[test]
    fn play_sends_edges_in_order() {
        let mut edges = Vec::new();
        let r: Result<(), ()> = play(Method::XTest.strokes().unwrap_or_default(), |k, down| {
            edges.push((k, down));
            Ok(())
        });
        assert_eq!(r, Ok(()));
        assert_eq!(
            edges,
            [
                (Key::Shift, true),
                (Key::Insert, true),
                (Key::Insert, false),
                (Key::Shift, false)
            ]
        );
    }

    #[test]
    fn missing_lists_required_tools_not_found() {
        assert_eq!(missing(Desktop::X11, |_| false), ["xclip"]);
        assert_eq!(missing(Desktop::Wayland, |_| false), ["wl-copy"]);
        assert_eq!(
            missing(Desktop::Wayland, |t| t == "wl-copy"),
            Vec::<&str>::new()
        );
        assert_eq!(missing(Desktop::Darwin, |_| false), Vec::<&str>::new());
    }

    #[test]
    fn restore_skips_empty_and_identical_clipboards() {
        assert_eq!(to_restore(Some("old"), "new"), Some("old"));
        assert_eq!(to_restore(Some(""), "new"), None);
        assert_eq!(to_restore(Some("new"), "new"), None);
        assert_eq!(to_restore(None, "new"), None);
    }

    /// Records every call; fails the methods listed in `broken`.
    #[derive(Default)]
    struct Fake {
        clipboard: Option<String>,
        broken: Vec<Method>,
        calls: Vec<String>,
    }

    impl Backend for Fake {
        fn read_clipboard(&mut self) -> Result<String, OutputError> {
            self.calls.push("read".into());
            self.clipboard.clone().ok_or(OutputError::Backend {
                action: Action::ReadClipboard,
                detail: "empty".into(),
            })
        }
        fn set_clipboard(&mut self, text: &str) -> Result<(), OutputError> {
            self.calls.push(format!("clipboard {text}"));
            self.clipboard = Some(text.to_owned());
            Ok(())
        }
        fn set_primary(&mut self, text: &str) -> Result<(), OutputError> {
            self.calls.push(format!("primary {text}"));
            Ok(())
        }
        fn release_primary(&mut self) {
            self.calls.push("release".into());
        }
        fn press(&mut self, method: Method) -> Result<(), OutputError> {
            self.calls.push(format!("press {}", method.label()));
            if self.broken.contains(&method) {
                return Err(OutputError::Permission {
                    what: crate::Permission::Uinput,
                    detail: "denied".into(),
                });
            }
            Ok(())
        }
        fn pause(&mut self, d: Duration) {
            self.calls.push(format!("pause {}", d.as_millis()));
        }
    }

    #[test]
    fn x11_restores_the_old_clipboard_after_the_paste_and_release() {
        let mut fake = Fake {
            clipboard: Some("old".into()),
            ..Fake::default()
        };
        let steps = plan(Desktop::X11, Delivery::Paste, PLAIN);
        assert!(execute(&mut fake, &steps, "new").is_ok());
        assert_eq!(
            fake.calls,
            [
                "read",
                "clipboard new",
                "primary new",
                "pause 50",
                "press x11/XTest+Shift+Insert",
                "pause 300",
                "release",
                "clipboard old",
            ]
        );
    }

    #[test]
    fn x11_leaves_an_empty_or_identical_clipboard_alone() {
        for before in [None, Some("same")] {
            let mut fake = Fake {
                clipboard: before.map(str::to_owned),
                ..Fake::default()
            };
            let steps = plan(Desktop::X11, Delivery::Paste, PLAIN);
            assert!(execute(&mut fake, &steps, "same").is_ok());
            assert_eq!(fake.calls.last().map(String::as_str), Some("release"));
            assert_eq!(fake.clipboard.as_deref(), Some("same"));
        }
    }

    #[test]
    fn a_failed_paste_keeps_the_text_and_gives_primary_up() {
        let mut fake = Fake {
            clipboard: Some("old".into()),
            broken: vec![Method::XTest],
            ..Fake::default()
        };
        let steps = plan(Desktop::X11, Delivery::Paste, PLAIN);
        let err = execute(&mut fake, &steps, "new");
        assert!(matches!(err, Err(OutputError::Permission { .. })));
        assert_eq!(
            fake.calls[3..],
            ["pause 50", "press x11/XTest+Shift+Insert", "release"]
        );
        assert_eq!(fake.clipboard.as_deref(), Some("new"));
    }

    #[test]
    fn a_failing_wtype_falls_back_to_uinput() {
        let mut fake = Fake {
            broken: vec![Method::Wtype],
            ..Fake::default()
        };
        let steps = plan(Desktop::Wayland, Delivery::Paste, PLAIN);
        assert!(execute(&mut fake, &steps, "hi").is_ok());
        assert_eq!(
            fake.calls,
            [
                "clipboard hi",
                "primary hi",
                "pause 50",
                "press wayland/wtype+Shift+Insert",
                "press wayland/uinput+Shift+Insert",
            ]
        );
    }

    #[test]
    fn the_last_methods_error_is_reported() {
        let mut fake = Fake {
            broken: vec![Method::Wtype, Method::Uinput],
            ..Fake::default()
        };
        let steps = plan(Desktop::Wayland, Delivery::Paste, PLAIN);
        let err = execute(&mut fake, &steps, "hi");
        assert!(matches!(err, Err(OutputError::Permission { .. })));
        assert_eq!(fake.calls.last().map(String::as_str), Some("release"));
    }
}
