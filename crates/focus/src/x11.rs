//! The X11 backend: the `WM_CLASS` of the root window's active window.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
use x11rb::rust_connection::RustConnection;

/// The longest `WM_CLASS` value read, in 32-bit units.
const CLASS_UNITS: u32 = 256;

/// The window class of the active window; `Ok(None)` when no window is active
/// or it names no class. Connects afresh on every call.
pub(crate) fn focused_app() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let (conn, screen) = RustConnection::connect(None)?;
    let root = conn.setup().roots.get(screen).ok_or("no such screen")?.root;

    let active = conn.intern_atom(true, b"_NET_ACTIVE_WINDOW")?.reply()?.atom;
    if active == x11rb::NONE {
        return Ok(None);
    }
    let reply = conn
        .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)?
        .reply()?;
    let Some(window) = reply.value32().and_then(|mut ids| ids.next()) else {
        return Ok(None);
    };
    if window == 0 {
        return Ok(None);
    }

    let reply = conn
        .get_property(
            false,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            0,
            CLASS_UNITS,
        )?
        .reply()?;
    Ok(class_of(&reply.value))
}

/// The class of a `WM_CLASS` value, two NUL-terminated strings (instance,
/// then class): the class, or the instance when there is no class.
fn class_of(value: &[u8]) -> Option<String> {
    let mut parts = value.split(|&b| b == 0).filter(|part| !part.is_empty());
    let first = parts.next()?;
    let class = parts.next().unwrap_or(first);
    Some(String::from_utf8_lossy(class).into_owned())
}

#[cfg(test)]
mod tests {
    use super::class_of;

    #[test]
    fn class_is_the_second_string() {
        assert_eq!(
            class_of(b"navigator\0Firefox\0").as_deref(),
            Some("Firefox")
        );
    }

    #[test]
    fn instance_stands_in_when_there_is_no_class() {
        assert_eq!(class_of(b"xterm\0").as_deref(), Some("xterm"));
    }

    #[test]
    fn empty_value_names_nothing() {
        assert_eq!(class_of(b""), None);
    }
}
