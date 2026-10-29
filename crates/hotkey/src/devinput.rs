//! The evdev backend: reads every keyboard under `/dev/input` without
//! grabbing it, so a hotkey also reaches the focused app (a grab taken
//! while a modifier is down hides its release from the compositor and
//! leaves the modifier stuck). Works in Wayland and X11 sessions and sees
//! Fn where the keyboard reports it; needs read access to the event nodes.
//! A keyboard plugged in later is picked up within [`RESCAN_MS`]; one
//! unplugged releases what it held.

use std::collections::{HashMap, HashSet};
use std::io::{self, PipeReader, PipeWriter};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use evdev::{Device, EventType};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

use crate::keymap::{self, Presses};
use crate::matcher::{Msg, Phys};
use crate::{Backend, HotkeyError};

const DIR: &str = "/dev/input";
/// How often the device list is read again, in milliseconds.
const RESCAN_MS: u16 = 2000;

/// A reader thread woken for shutdown through a pipe. Dropping it closes
/// the pipe, which the thread's `poll` sees, and joins the thread.
pub(crate) struct Reader {
    wake: Option<PipeWriter>,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// Runs `run` on a thread named `name`; `run` returns once its pipe end
    /// reports anything.
    pub(crate) fn spawn(
        name: &str,
        run: impl FnOnce(PipeReader) + Send + 'static,
    ) -> Result<Reader, HotkeyError> {
        let (wake_rx, wake_tx) = io::pipe().map_err(backend)?;
        let thread = thread::Builder::new()
            .name(name.into())
            .spawn(move || run(wake_rx))
            .map_err(backend)?;
        Ok(Reader {
            wake: Some(wake_tx),
            thread: Some(thread),
        })
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        drop(self.wake.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn backend(e: impl std::fmt::Display) -> HotkeyError {
    HotkeyError::Backend(e.to_string())
}

/// Whether a polled fd reported anything.
pub(crate) fn woke(fd: &PollFd<'_>) -> bool {
    fd.revents().is_some_and(|r| !r.is_empty())
}

pub(crate) fn start(tx: mpsc::Sender<Msg>) -> Result<Backend, HotkeyError> {
    let mut devices = Devices::default();
    let scan = devices
        .scan()
        .map_err(|e| HotkeyError::Backend(format!("read {DIR}: {e}")))?;
    if scan.found == 0 {
        return Err(HotkeyError::NoInputDevices);
    }
    if scan.readable == 0 {
        return Err(if scan.denied > 0 {
            HotkeyError::InputDenied
        } else {
            HotkeyError::Backend(format!("could not open any device under {DIR}"))
        });
    }
    let reader = Reader::spawn("anya-hotkey-evdev", move |wake| {
        run(devices, &wake, &tx);
    })?;
    Ok(Backend::new("evdev", keymap::EVDEV, reader))
}

struct Open {
    path: PathBuf,
    device: Device,
    /// Key codes this device holds, with the key each was read as.
    held: HashMap<u16, Phys>,
}

#[derive(Default)]
struct Devices {
    open: Vec<Open>,
    /// Nodes opened before (keyboards or not); a denied node is retried.
    tried: HashSet<PathBuf>,
    presses: Presses,
}

#[derive(Debug, Default)]
struct Scan {
    found: usize,
    readable: usize,
    denied: usize,
}

impl Devices {
    /// Opens the keyboards not seen before.
    fn scan(&mut self) -> io::Result<Scan> {
        let mut scan = Scan::default();
        let mut present = HashSet::new();
        for entry in std::fs::read_dir(DIR)?.flatten() {
            let path = entry.path();
            let is_event = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"));
            if !is_event {
                continue;
            }
            scan.found += 1;
            present.insert(path.clone());
            if self.tried.contains(&path) {
                scan.readable += 1;
                continue;
            }
            match Device::open(&path) {
                Ok(device) => {
                    scan.readable += 1;
                    self.tried.insert(path.clone());
                    if !is_keyboard(&device) {
                        continue;
                    }
                    if let Err(e) = device.set_nonblocking(true) {
                        tracing::warn!(path = %path.display(), error = %e, "cannot read input device");
                        continue;
                    }
                    tracing::debug!(path = %path.display(), name = device.name().unwrap_or(""), "reading keyboard");
                    self.open.push(Open {
                        path,
                        device,
                        held: HashMap::new(),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => scan.denied += 1,
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "cannot open input device");
                }
            }
        }
        self.tried.retain(|p| present.contains(p));
        Ok(scan)
    }

    /// Reads what device `i` has; false when it is gone.
    fn read(&mut self, i: usize, tx: &mpsc::Sender<Msg>) -> bool {
        let Devices { open, presses, .. } = self;
        let dev = &mut open[i];
        loop {
            let events = match dev.device.fetch_events() {
                Ok(events) => events,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    tracing::debug!(path = %dev.path.display(), error = %e, "input device gone");
                    return false;
                }
            };
            for ev in events {
                if ev.event_type() != EventType::KEY {
                    continue;
                }
                let code = ev.code();
                match ev.value() {
                    1 => {
                        if dev.held.contains_key(&code) {
                            continue;
                        }
                        let Some(p) = keymap::lookup(keymap::EVDEV, u32::from(code)) else {
                            continue;
                        };
                        dev.held.insert(code, p);
                        if presses.press(p) {
                            let _ = tx.send(Msg::Down(p));
                        }
                    }
                    0 => {
                        if let Some(p) = dev.held.remove(&code)
                            && presses.release(p)
                        {
                            let _ = tx.send(Msg::Up(p));
                        }
                    }
                    // 2 is auto-repeat.
                    _ => {}
                }
            }
        }
    }

    /// Drops device `i`, releasing what it held.
    fn remove(&mut self, i: usize, tx: &mpsc::Sender<Msg>) {
        let gone = self.open.swap_remove(i);
        self.tried.remove(&gone.path);
        for p in gone.held.into_values() {
            if self.presses.release(p) {
                let _ = tx.send(Msg::Up(p));
            }
        }
    }
}

/// A device that has a key a hotkey can name: keyboards, not mice.
fn is_keyboard(device: &Device) -> bool {
    device.supported_keys().is_some_and(|keys| {
        keys.iter()
            .any(|k| keymap::lookup(keymap::EVDEV, u32::from(k.0)).is_some())
    })
}

fn run(mut devices: Devices, wake: &PipeReader, tx: &mpsc::Sender<Msg>) {
    let rescan = Duration::from_millis(u64::from(RESCAN_MS));
    let mut last_scan = Instant::now();
    loop {
        let ready: Vec<bool> = {
            let mut fds = Vec::with_capacity(devices.open.len() + 1);
            fds.push(PollFd::new(wake.as_fd(), PollFlags::POLLIN));
            for d in &devices.open {
                fds.push(PollFd::new(d.device.as_fd(), PollFlags::POLLIN));
            }
            match poll(&mut fds, PollTimeout::from(RESCAN_MS)) {
                Ok(_) => {}
                Err(Errno::EINTR) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "polling input devices failed");
                    break;
                }
            }
            fds.iter().map(woke).collect()
        };
        if ready[0] {
            break;
        }
        let gone: Vec<usize> = ready[1..]
            .iter()
            .enumerate()
            .filter(|&(i, &r)| r && !devices.read(i, tx))
            .map(|(i, _)| i)
            .collect();
        for &i in gone.iter().rev() {
            devices.remove(i, tx);
        }
        if last_scan.elapsed() >= rescan {
            last_scan = Instant::now();
            if let Err(e) = devices.scan() {
                tracing::warn!(error = %e, "rescanning {DIR} failed");
            }
        }
    }
    let _ = tx.send(Msg::Closed);
}
