//! Shift+Insert faked through the X server's XTEST extension.

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Keycode, Keysym,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};

use crate::plan::{Key, Stroke, play};
use crate::{Action, OutputError};

/// `XK_Shift_L`.
const SHIFT_L: Keysym = 0xffe1;
/// `XK_Insert`.
const INSERT: Keysym = 0xff63;

fn failed(detail: impl std::fmt::Display) -> OutputError {
    OutputError::Backend {
        action: Action::Paste,
        detail: detail.to_string(),
    }
}

/// Plays `strokes` on the display `$DISPLAY` names.
pub(crate) fn press(strokes: &[Stroke]) -> Result<(), OutputError> {
    let (conn, _) =
        x11rb::connect(None).map_err(|e| failed(format!("cannot open X display: {e}")))?;
    if conn
        .extension_information(xtest::X11_EXTENSION_NAME)
        .map_err(failed)?
        .is_none()
    {
        return Err(failed("the X server has no XTEST extension"));
    }

    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn
        .get_keyboard_mapping(min, max.saturating_sub(min).saturating_add(1))
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    let code = |sym, name| {
        keycode(&map.keysyms, map.keysyms_per_keycode, min, sym)
            .ok_or_else(|| failed(format!("no key produces {name} in this keymap")))
    };
    let shift = code(SHIFT_L, "Shift_L")?;
    let insert = code(INSERT, "Insert")?;

    play(strokes, |key, down| {
        let detail = match key {
            Key::Shift => shift,
            Key::Insert => insert,
            Key::Command | Key::V => return Err(failed(format!("{key:?} is not an X11 key"))),
        };
        let kind = if down {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        };
        conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, x11rb::NONE, 0, 0, 0)
            .map_err(failed)?;
        conn.flush().map_err(failed)
    })?;
    // A round trip, so the server has taken every event before we hang up.
    conn.get_input_focus()
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    Ok(())
}

/// The first keycode whose mapping holds `sym`, as `XKeysymToKeycode` finds
/// it: `keysyms` has `per` entries for each keycode from `min` on.
fn keycode(keysyms: &[Keysym], per: u8, min: Keycode, sym: Keysym) -> Option<Keycode> {
    if per == 0 {
        return None;
    }
    let index = keysyms
        .chunks(usize::from(per))
        .position(|syms| syms.contains(&sym))?;
    u8::try_from(index).ok()?.checked_add(min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycodes_come_from_the_mapping_row_holding_the_keysym() {
        // Keycodes 8.. with two keysyms each; Shift_L on 50, Insert on 118.
        let mut syms = vec![0; 2 * 248];
        syms[2 * (50 - 8)] = SHIFT_L;
        syms[2 * (118 - 8) + 1] = INSERT;
        assert_eq!(keycode(&syms, 2, 8, SHIFT_L), Some(50));
        assert_eq!(keycode(&syms, 2, 8, INSERT), Some(118));
        assert_eq!(keycode(&syms, 2, 8, 0xffe2), None);
        assert_eq!(keycode(&syms, 0, 8, SHIFT_L), None);
    }
}
