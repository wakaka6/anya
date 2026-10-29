//! The X11 backend: XInput2 raw key events selected on the root window.
//! Raw events reach every client that selects them, also while another
//! client holds a grab, and take nothing from the focused app, so a
//! hotkey neither swallows keys nor dies silently on a grab conflict. X11
//! has no keysym for Fn; the evdev backend sees it.

use std::collections::HashMap;
use std::io::PipeReader;
use std::os::fd::AsFd;
use std::sync::mpsc;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use crate::devinput::{Reader, backend, woke};
use crate::keymap::{self, Presses};
use crate::matcher::{Msg, Phys};
use crate::{Backend, HotkeyError};

/// `XIAllMasterDevices`.
const ALL_MASTER_DEVICES: u16 = 1;

pub(crate) fn start(tx: mpsc::Sender<Msg>) -> Result<Backend, HotkeyError> {
    let (conn, screen) = x11rb::connect(None).map_err(|_| {
        HotkeyError::NoDisplay(std::env::var("DISPLAY").unwrap_or_else(|_| "(unset)".into()))
    })?;
    let root = conn
        .setup()
        .roots
        .get(screen)
        .map(|s| s.root)
        .ok_or_else(|| backend("the X display has no screen"))?;
    if conn
        .extension_information(xinput::X11_EXTENSION_NAME)
        .map_err(backend)?
        .is_none()
    {
        return Err(backend("the X server has no XInput extension"));
    }
    let version = conn
        .xinput_xi_query_version(2, 2)
        .map_err(backend)?
        .reply()
        .map_err(backend)?;
    if version.major_version < 2 {
        return Err(backend(format!(
            "the X server has XInput {}.{}; XInput 2 is needed",
            version.major_version, version.minor_version
        )));
    }
    let mask = xinput::EventMask {
        deviceid: ALL_MASTER_DEVICES,
        mask: vec![xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE],
    };
    conn.xinput_xi_select_events(root, &[mask])
        .map_err(backend)?
        .check()
        .map_err(backend)?;
    let state = State {
        keys: load_keymap(&conn)?,
        held: HashMap::new(),
        presses: Presses::default(),
    };
    let reader = Reader::spawn("anya-hotkey-x11", move |wake| {
        run(&conn, state, &wake, &tx);
    })?;
    Ok(Backend::new("XInput2", keymap::X11, reader))
}

/// The physical key of every keycode in the current keyboard mapping.
fn load_keymap(conn: &RustConnection) -> Result<HashMap<u8, Phys>, HotkeyError> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let count = max.saturating_sub(min).saturating_add(1);
    let reply = conn
        .get_keyboard_mapping(min, count)
        .map_err(backend)?
        .reply()
        .map_err(backend)?;
    Ok(keymap::x11_keycodes(
        min,
        reply.keysyms_per_keycode,
        &reply.keysyms,
    ))
}

struct State {
    keys: HashMap<u8, Phys>,
    /// Keycodes held, with the key each was read as.
    held: HashMap<u8, Phys>,
    presses: Presses,
}

fn run(conn: &RustConnection, mut state: State, wake: &PipeReader, tx: &mpsc::Sender<Msg>) {
    'listen: loop {
        loop {
            match conn.poll_for_event() {
                Ok(Some(event)) => state.handle(conn, event, tx),
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "the X connection failed");
                    break 'listen;
                }
            }
        }
        if let Err(e) = conn.flush() {
            tracing::warn!(error = %e, "the X connection failed");
            break;
        }
        let mut fds = [
            PollFd::new(wake.as_fd(), PollFlags::POLLIN),
            PollFd::new(conn.stream().as_fd(), PollFlags::POLLIN),
        ];
        match poll(&mut fds, PollTimeout::NONE) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "polling the X connection failed");
                break;
            }
        }
        if woke(&fds[0]) {
            break;
        }
    }
    let _ = tx.send(Msg::Closed);
}

impl State {
    fn handle(&mut self, conn: &RustConnection, event: Event, tx: &mpsc::Sender<Msg>) {
        match event {
            Event::XinputRawKeyPress(e) => {
                let repeat = u32::from(e.flags) & u32::from(xinput::KeyEventFlags::KEY_REPEAT);
                let Ok(code) = u8::try_from(e.detail) else {
                    return;
                };
                if repeat != 0 || self.held.contains_key(&code) {
                    return;
                }
                let Some(&p) = self.keys.get(&code) else {
                    return;
                };
                self.held.insert(code, p);
                if self.presses.press(p) {
                    let _ = tx.send(Msg::Down(p));
                }
            }
            Event::XinputRawKeyRelease(e) => {
                if let Ok(code) = u8::try_from(e.detail)
                    && let Some(p) = self.held.remove(&code)
                    && self.presses.release(p)
                {
                    let _ = tx.send(Msg::Up(p));
                }
            }
            Event::MappingNotify(e) if e.request == xproto::Mapping::KEYBOARD => {
                match load_keymap(conn) {
                    Ok(keys) => self.keys = keys,
                    Err(e) => tracing::warn!(error = %e, "reloading the X keyboard mapping failed"),
                }
            }
            _ => {}
        }
    }
}
