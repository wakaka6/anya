//! Global hotkeys on macOS, Wayland and X11.
//!
//! [`Hotkeys::start`] opens the backend for a desktop: a CGEventTap on macOS,
//! the evdev nodes under `/dev/input` on Wayland, XInput2 raw key events on
//! X11. Every backend only listens: a hotkey also reaches the focused app.
//! The OS callback records which physical key went down or up and returns;
//! one matcher thread turns those edges into [`Event`]s for the registered
//! [`Combo`]s, so every backend matches the same way:
//!
//! - a combo goes [`Edge::Down`] on the press that makes it held, with the
//!   held modifiers exactly the combo's (`Alt+Super` does not fire while
//!   Ctrl is held too), and [`Edge::Up`] once when any of its keys is
//!   released, whichever goes first;
//! - a key held down repeats nothing;
//! - left and right modifier keys are one modifier.

mod combo;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod darwin;
#[cfg(target_os = "linux")]
mod devinput;
mod doctor;
mod keymap;
mod matcher;
#[cfg(target_os = "linux")]
mod xi2;

use std::any::Any;
use std::fmt;
use std::sync::mpsc;

use anya_core::desktop::Desktop;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

pub use combo::{Combo, Disallowed, Key, Mods, ParseError, format_list, parse_list};
pub use doctor::checks;

use keymap::Table;
use matcher::Msg;

/// Which way a combo went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    /// The combo became held.
    Down,
    /// The combo stopped being held.
    Up,
}

/// A registered combo went down or up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Event {
    /// The combo, as registered.
    pub combo: Combo,
    /// Which way it went.
    pub edge: Edge,
}

/// Why the backend cannot listen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyError {
    /// macOS has not granted Anya Accessibility, which the event tap needs.
    /// Starting asked macOS to show its prompt.
    #[error("macOS Accessibility permission is required for global hotkeys")]
    AccessibilityDenied,
    /// No keyboard under `/dev/input` is readable: the user is not in the
    /// `input` group, or has not logged in again since joining it.
    #[error(
        "cannot read any device under /dev/input; add the user to the input group or grant read access to /dev/input/event*"
    )]
    InputDenied,
    /// `/dev/input` holds no event device.
    #[error("no input event devices found under /dev/input")]
    NoInputDevices,
    /// The X display named by `DISPLAY` cannot be opened.
    #[error("cannot open X display (DISPLAY={0})")]
    NoDisplay(String),
    /// This build has no backend for the desktop.
    #[error("global hotkeys are not available on {0}")]
    UnsupportedDesktop(Desktop),
    /// The backend failed for another reason.
    #[error("hotkey backend failed: {0}")]
    Backend(String),
}

impl HotkeyError {
    /// A stable code for the app to translate.
    pub fn code(&self) -> &'static str {
        match self {
            HotkeyError::AccessibilityDenied => "hotkey_accessibility_denied",
            HotkeyError::InputDenied => "hotkey_input_denied",
            HotkeyError::NoInputDevices => "hotkey_no_input_devices",
            HotkeyError::NoDisplay(_) => "hotkey_no_display",
            HotkeyError::UnsupportedDesktop(_) => "hotkey_unsupported_desktop",
            HotkeyError::Backend(_) => "hotkey_backend",
        }
    }

    /// Whether the user can fix it by granting a permission.
    pub fn is_permission(&self) -> bool {
        matches!(
            self,
            HotkeyError::AccessibilityDenied | HotkeyError::InputDenied
        )
    }
}

/// Why a backend cannot see a combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The backend never reports this key (CapsLock and F21 to F24 on macOS).
    KeyUnseen(Key),
    /// The backend never reports this modifier (Fn on X11; evdev sees it).
    ModifierUnseen(Mods),
}

impl Reason {
    /// A stable code for the app to translate.
    pub fn code(&self) -> &'static str {
        match self {
            Reason::KeyUnseen(_) => "hotkey_key_unseen",
            Reason::ModifierUnseen(_) => "hotkey_modifier_unseen",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::KeyUnseen(key) => write!(f, "the backend cannot see the {key} key"),
            Reason::ModifierUnseen(mods) => write!(f, "the backend cannot see {mods}"),
        }
    }
}

/// Combos [`Hotkeys::set`] registered but the backend can never report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.describe())]
pub struct Unsupported(pub Vec<(Combo, Reason)>);

impl Unsupported {
    fn describe(&self) -> String {
        self.0
            .iter()
            .map(|(combo, reason)| format!("hotkey {combo}: {reason}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The listening backend. Dropping it stops listening.
struct Backend {
    name: &'static str,
    table: Table,
    _handle: Box<dyn Any + Send + Sync>,
}

impl Backend {
    fn new(name: &'static str, table: Table, handle: impl Any + Send + Sync) -> Backend {
        Backend {
            name,
            table,
            _handle: Box::new(handle),
        }
    }
}

/// Global hotkeys. Dropping it stops listening and ends the event stream.
pub struct Hotkeys {
    tx: mpsc::Sender<Msg>,
    backend: Backend,
}

impl Hotkeys {
    /// Starts listening on `desktop` and returns the edges of registered
    /// combos. Nothing is registered yet: call [`Hotkeys::set`].
    ///
    /// [`Desktop::Wayland`] reads evdev, which also works in an X11 session
    /// and is the backend that sees Fn on Linux. On macOS, a missing
    /// Accessibility grant fails with [`HotkeyError::AccessibilityDenied`]
    /// after asking macOS to show its prompt.
    ///
    /// The receiver ends when the backend stops: the `Hotkeys` was dropped,
    /// or the device or display went away.
    pub fn start(
        desktop: Desktop,
    ) -> Result<(Hotkeys, tokio_mpsc::UnboundedReceiver<Event>), HotkeyError> {
        let (events, rx) = tokio_mpsc::unbounded_channel();
        let tx = matcher::spawn(events).map_err(|e| HotkeyError::Backend(e.to_string()))?;
        let backend = open(desktop, tx.clone())?;
        Ok((Hotkeys { tx, backend }, rx))
    }

    /// Replaces the registered combos with `combos`, repeats dropped. A held
    /// combo that stays registered stays held; one that is removed goes
    /// [`Edge::Up`]; one that is added fires on its next press.
    ///
    /// Every combo is registered; the ones this backend can never report are
    /// returned in [`Unsupported`].
    pub fn set(&self, combos: &[Combo]) -> Result<(), Unsupported> {
        let mut unique: Vec<Combo> = Vec::with_capacity(combos.len());
        for &combo in combos {
            if !unique.contains(&combo) {
                unique.push(combo);
            }
        }
        let unsupported: Vec<(Combo, Reason)> = unique
            .iter()
            .filter_map(|&c| keymap::unseen(self.backend.table, c).map(|r| (c, r)))
            .collect();
        // The matcher thread ends only with the backend; then nothing fires.
        let _ = self.tx.send(Msg::Set(unique));
        if unsupported.is_empty() {
            Ok(())
        } else {
            Err(Unsupported(unsupported))
        }
    }

    /// Captures the next combo the user presses, for a settings page. While
    /// recording, registered combos are paused (a held one goes
    /// [`Edge::Up`]). A key that is not a modifier completes the combo on its
    /// press, with the modifiers then held; modifiers alone complete when one
    /// of them is released. Esc with no modifier cancels.
    ///
    /// Resolves to `None` when cancelled, when another recording starts, or
    /// when the backend stops. Dropping the future cancels the recording and
    /// resumes the hotkeys. The result is not checked with
    /// [`Combo::check_global`].
    pub fn record(&self) -> impl Future<Output = Option<Combo>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Msg::Record(tx));
        async move { rx.await.ok().flatten() }
    }

    /// The backend's name for logs: `CGEventTap`, `evdev` or `XInput2`.
    pub fn backend(&self) -> &'static str {
        self.backend.name
    }
}

impl fmt::Debug for Hotkeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hotkeys")
            .field("backend", &self.backend.name)
            .finish_non_exhaustive()
    }
}

fn open(desktop: Desktop, tx: mpsc::Sender<Msg>) -> Result<Backend, HotkeyError> {
    match desktop {
        #[cfg(target_os = "macos")]
        Desktop::Darwin => darwin::start(tx),
        #[cfg(target_os = "linux")]
        Desktop::Wayland => devinput::start(tx),
        #[cfg(target_os = "linux")]
        Desktop::X11 => xi2::start(tx),
        other => {
            drop(tx);
            Err(HotkeyError::UnsupportedDesktop(other))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matcher::{Phys, Side};
    use std::time::Duration;
    use tokio::time::timeout;

    /// Hotkeys over a fake backend that sees what the macOS table sees;
    /// the returned sender plays the OS callback.
    fn fake() -> (
        Hotkeys,
        mpsc::Sender<Msg>,
        tokio_mpsc::UnboundedReceiver<Event>,
    ) {
        let (events, rx) = tokio_mpsc::unbounded_channel();
        let tx = matcher::spawn(events).expect("spawn the matcher");
        let backend = Backend::new("fake", keymap::MAC, ());
        (
            Hotkeys {
                tx: tx.clone(),
                backend,
            },
            tx,
            rx,
        )
    }

    async fn next(rx: &mut tokio_mpsc::UnboundedReceiver<Event>) -> Option<Event> {
        timeout(Duration::from_secs(2), rx.recv())
            .await
            .ok()
            .flatten()
    }

    fn combo(s: &str) -> Combo {
        s.parse().expect("a combo")
    }

    const ALT: Phys = Phys::Mod(Mods::ALT, Side::Left);
    const SUPER: Phys = Phys::Mod(Mods::SUPER, Side::Left);

    #[tokio::test]
    async fn edges_reach_the_receiver_and_unseen_combos_are_named() {
        let (hotkeys, os, mut rx) = fake();
        let set = hotkeys.set(&[combo("Alt+Super"), combo("CapsLock"), combo("Alt+Super")]);
        assert_eq!(
            set,
            Err(Unsupported(vec![(
                combo("CapsLock"),
                Reason::KeyUnseen(Key::CapsLock)
            )]))
        );
        for msg in [
            Msg::Down(ALT),
            Msg::Down(SUPER),
            Msg::Up(SUPER),
            Msg::Up(ALT),
        ] {
            os.send(msg).expect("matcher running");
        }
        let alt_super = combo("Alt+Super");
        assert_eq!(
            next(&mut rx).await,
            Some(Event {
                combo: alt_super,
                edge: Edge::Down
            })
        );
        assert_eq!(
            next(&mut rx).await,
            Some(Event {
                combo: alt_super,
                edge: Edge::Up
            })
        );
        os.send(Msg::Closed).expect("matcher running");
        assert_eq!(
            next(&mut rx).await,
            None,
            "the stream ends with the backend"
        );
    }

    #[tokio::test]
    async fn recording_captures_a_combo_and_dropping_it_resumes_hotkeys() {
        let (hotkeys, os, mut rx) = fake();
        hotkeys.set(&[combo("F13")]).expect("F13 is seen");
        let recording = hotkeys.record();
        for msg in [
            Msg::Down(ALT),
            Msg::Down(SUPER),
            Msg::Up(ALT),
            Msg::Up(SUPER),
        ] {
            os.send(msg).expect("matcher running");
        }
        assert_eq!(recording.await, Some(combo("Alt+Super")));

        let abandoned = hotkeys.record();
        drop(abandoned);
        // The matcher notices the dropped recording within its poll interval.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let f13 = Phys::Key(Key::F13);
        os.send(Msg::Down(f13)).expect("matcher running");
        assert_eq!(
            next(&mut rx).await,
            Some(Event {
                combo: combo("F13"),
                edge: Edge::Down
            })
        );

        let first = hotkeys.record();
        let second = hotkeys.record();
        assert_eq!(first.await, None, "a new recording supersedes the old one");
        os.send(Msg::Down(Phys::Key(Key::Escape)))
            .expect("matcher running");
        assert_eq!(second.await, None, "Esc cancels");
    }
}
