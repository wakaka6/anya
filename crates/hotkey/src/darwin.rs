//! The macOS backend: a CGEventTap on its own thread and run loop. It sits
//! at the session level and passes every event on unchanged, so a hotkey
//! also reaches the focused app. The callback only translates the key
//! code and sends the edge to the matcher thread.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use anya_core::doctor::{Check, Severity};
use objc2_application_services::{
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFMachPort, CFRetained, CFRunLoop, CFRunLoopRunResult, CFString,
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

use crate::keymap;
use crate::matcher::{Msg, Phys};
use crate::{Backend, HotkeyError};

/// Whether macOS trusts this process with Accessibility; when it does not
/// and `prompt` is set, asks macOS to show its prompt.
fn trusted(prompt: bool) -> bool {
    // SAFETY: AXIsProcessTrusted takes no arguments and only reads state.
    if unsafe { AXIsProcessTrusted() } {
        return true;
    }
    if !prompt {
        return false;
    }
    // SAFETY: an immutable CFString constant exported by ApplicationServices.
    let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let options = CFDictionary::from_slices(&[key], &[CFBoolean::new(true)]);
    // SAFETY: the options map CFString keys to CFBoolean values, the types
    // AXIsProcessTrustedWithOptions documents.
    unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) }
}

/// The `accessibility` doctor check; a denial shows macOS's prompt.
pub(crate) fn accessibility_check() -> Check {
    const CODE: &str = "accessibility";
    if trusted(true) {
        Check::passed(CODE, Severity::Required, "granted")
    } else {
        Check::failed(CODE, Severity::Required, "denied")
    }
}

/// What the tap callback reads. Owned by the tap thread.
struct TapState {
    tx: mpsc::Sender<Msg>,
    /// The tap's port, to re-enable it when macOS disables it.
    port: Cell<*const CFMachPort>,
}

/// The tap thread's run loop, for stopping it from another thread.
struct LoopHandle(CFRetained<CFRunLoop>);

// SAFETY: the handle only calls CFRunLoopStop and releases its reference;
// Core Foundation allows both from any thread.
unsafe impl Send for LoopHandle {}
// SAFETY: as for Send: CFRunLoopStop is thread-safe and takes `&self`.
unsafe impl Sync for LoopHandle {}

/// The running tap. Dropping it stops the run loop and joins the thread.
struct Tap {
    stop: Arc<AtomicBool>,
    run_loop: LoopHandle,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.run_loop.0.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Ready = mpsc::SyncSender<Result<LoopHandle, HotkeyError>>;

/// Starts the tap. Without Accessibility, asks macOS for it and fails.
pub(crate) fn start(tx: mpsc::Sender<Msg>) -> Result<Backend, HotkeyError> {
    if !trusted(true) {
        return Err(HotkeyError::AccessibilityDenied);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("anya-hotkey-tap".into())
        .spawn(move || run(tx, &thread_stop, &ready_tx))
        .map_err(|e| HotkeyError::Backend(e.to_string()))?;
    let run_loop = match ready_rx.recv() {
        Ok(Ok(run_loop)) => run_loop,
        Ok(Err(e)) => {
            let _ = thread.join();
            return Err(e);
        }
        Err(_) => {
            let _ = thread.join();
            return Err(HotkeyError::Backend("the event tap thread ended".into()));
        }
    };
    let tap = Tap {
        stop,
        run_loop,
        thread: Some(thread),
    };
    Ok(Backend::new("CGEventTap", keymap::MAC, tap))
}

fn run(tx: mpsc::Sender<Msg>, stop: &AtomicBool, ready: &Ready) {
    let state = Box::into_raw(Box::new(TapState {
        tx,
        port: Cell::new(ptr::null()),
    }));
    let mask: CGEventMask = (1 << CGEventType::KeyDown.0)
        | (1 << CGEventType::KeyUp.0)
        | (1 << CGEventType::FlagsChanged.0);
    // SAFETY: `callback` has the CGEventTapCallBack signature, and `state`
    // stays valid until the tap is invalidated below; after that the
    // callback is not called again.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(callback),
            state.cast(),
        )
    };
    let Some(port) = port else {
        // SAFETY: no tap was created, so nothing else refers to `state`.
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err(HotkeyError::Backend(
            "CGEventTapCreate failed; check Accessibility and Input Monitoring".into(),
        )));
        return;
    };
    // SAFETY: the callback runs only on this thread, inside the run loop
    // below, so nothing reads `state` while it is written here.
    unsafe { (*state).port.set(CFRetained::as_ptr(&port).as_ptr()) };

    let setup = CFMachPort::new_run_loop_source(None, Some(&port), 0).zip(CFRunLoop::current());
    let Some((source, run_loop)) = setup else {
        port.invalidate();
        // SAFETY: the tap is invalidated, so its callback cannot run again.
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err(HotkeyError::Backend(
            "CFMachPortCreateRunLoopSource failed".into(),
        )));
        return;
    };
    // SAFETY: immutable run loop mode constants exported by Core Foundation.
    let (common, default) = unsafe { (kCFRunLoopCommonModes, kCFRunLoopDefaultMode) };
    run_loop.add_source(Some(&source), common);
    CGEvent::tap_enable(&port, true);

    if ready.send(Ok(LoopHandle(run_loop.clone()))).is_ok() {
        // A stop that lands before the loop runs is seen within a second.
        while !stop.load(Ordering::SeqCst) {
            let result = CFRunLoop::run_in_mode(default, 1.0, false);
            if result.0 == CFRunLoopRunResult::Finished.0 {
                break;
            }
        }
    }

    CGEvent::tap_enable(&port, false);
    source.invalidate();
    port.invalidate();
    drop(source);
    drop(port);
    // SAFETY: the tap is invalidated, so the callback that borrowed `state`
    // cannot run again.
    let state = unsafe { Box::from_raw(state) };
    let _ = state.tx.send(Msg::Closed);
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user_info` is the TapState `run` keeps alive while the tap
    // exists, and only this thread touches it.
    let state = unsafe { &*user_info.cast::<TapState>() };
    let msgs = if kind == CGEventType::TapDisabledByTimeout
        || kind == CGEventType::TapDisabledByUserInput
    {
        let port = state.port.get();
        if !port.is_null() {
            // SAFETY: `port` is this tap's port, alive while its callback runs.
            CGEvent::tap_enable(unsafe { &*port }, true);
        }
        // Edges were missed while the tap was off.
        [Some(Msg::Reset), None]
    } else {
        // SAFETY: macOS passes a valid event for the duration of the callback.
        translate(kind, unsafe { event.as_ref() })
    };
    for msg in msgs.into_iter().flatten() {
        let _ = state.tx.send(msg);
    }
    event.as_ptr()
}

/// The edges an event means, in order. Every event also carries the
/// modifiers held, which corrects a modifier edge that never arrived (held
/// before the tap started, missed while it was off, or posted by software
/// as a flag on the key): before a key's press, and after a modifier's edge.
fn translate(kind: CGEventType, event: &CGEvent) -> [Option<Msg>; 2] {
    let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode);
    let Some(p) = u32::try_from(code)
        .ok()
        .and_then(|code| keymap::lookup(keymap::MAC, code))
    else {
        return [None, None];
    };
    let is_key = matches!(p, Phys::Key(_));
    let flags = CGEvent::flags(Some(event)).0;
    let sync = Msg::Sync(keymap::mac_flag_mods(flags));
    if kind == CGEventType::KeyDown {
        let repeat =
            CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventAutorepeat) != 0;
        if is_key && !repeat {
            [Some(sync), Some(Msg::Down(p))]
        } else {
            [None, None]
        }
    } else if kind == CGEventType::KeyUp {
        [is_key.then_some(Msg::Up(p)), None]
    } else if kind == CGEventType::FlagsChanged && !is_key {
        let edge = if keymap::mac_modifier_down(p, flags) {
            Msg::Down(p)
        } else {
            Msg::Up(p)
        };
        [Some(edge), Some(sync)]
    } else {
        [None, None]
    }
}
