//! Shift+Insert from a virtual keyboard on `/dev/uinput`, which every Wayland
//! compositor reads like a real one.

use std::io;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, KeyCode, KeyEvent};

use crate::plan::{Key, Stroke, play};
use crate::{Action, OutputError, Permission};

/// The name the device shows in `libinput list-devices`.
const NAME: &str = "Anya virtual keyboard";

/// The keys the device declares: the range a full keyboard has, so udev and
/// libinput classify it as a keyboard and not as a button box.
const KEYS: std::ops::RangeInclusive<u16> = 1..=248;

fn failed(detail: impl std::fmt::Display) -> OutputError {
    OutputError::Backend {
        action: Action::Paste,
        detail: detail.to_string(),
    }
}

fn open_error(e: io::Error) -> OutputError {
    match e.kind() {
        io::ErrorKind::PermissionDenied => OutputError::Permission {
            what: Permission::Uinput,
            detail: e.to_string(),
        },
        io::ErrorKind::NotFound => failed(format!(
            "create uinput keyboard: /dev/uinput does not exist (is the uinput module loaded?): {e}"
        )),
        _ => failed(format!("create uinput keyboard: {e}")),
    }
}

fn code(key: Key) -> Result<KeyCode, OutputError> {
    match key {
        Key::Shift => Ok(KeyCode::KEY_LEFTSHIFT),
        Key::Insert => Ok(KeyCode::KEY_INSERT),
        Key::Command | Key::V => Err(failed(format!("{key:?} is not a Linux key"))),
    }
}

/// Creates a keyboard, plays `strokes` on it and removes it again.
pub(crate) fn press(strokes: &[Stroke]) -> Result<(), OutputError> {
    let keys: AttributeSet<KeyCode> = KEYS.map(KeyCode).collect();
    let mut device = VirtualDevice::builder()
        .and_then(|b| b.name(NAME).with_keys(&keys))
        .and_then(|b| b.build())
        .map_err(open_error)?;
    play(strokes, |key, down| {
        let event = *KeyEvent::new(code(key)?, i32::from(down));
        device.emit(&[event]).map_err(|e| {
            let edge = if down { "down" } else { "up" };
            failed(format!("uinput {key:?} {edge}: {e}"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_declares_shift_and_insert() {
        assert!(KEYS.contains(&KeyCode::KEY_LEFTSHIFT.0));
        assert!(KEYS.contains(&KeyCode::KEY_INSERT.0));
        assert_eq!(code(Key::Shift), Ok(KeyCode::KEY_LEFTSHIFT));
        assert_eq!(code(Key::Insert), Ok(KeyCode::KEY_INSERT));
    }

    #[test]
    fn a_denied_device_is_a_permission_error() {
        let denied = open_error(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(matches!(
            denied,
            OutputError::Permission {
                what: Permission::Uinput,
                ..
            }
        ));
        let absent = open_error(io::Error::from(io::ErrorKind::NotFound));
        assert!(matches!(
            absent,
            OutputError::Backend {
                action: Action::Paste,
                ..
            }
        ));
    }

    /// Needs write access to /dev/uinput and types Shift+Insert into the
    /// focused window: `cargo test -p anya-output -- --ignored uinput_types`.
    #[test]
    #[ignore]
    fn uinput_types_shift_insert() {
        let strokes = crate::plan::Method::Uinput.strokes().unwrap_or_default();
        assert_eq!(press(strokes), Ok(()));
    }
}
