//! macOS: the general pasteboard and Cmd+V posted with CGEvent.

use std::time::Duration;

use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode, CGPreflightPostEventAccess,
    CGRequestPostEventAccess,
};
use objc2_foundation::NSString;

use crate::plan::{Backend, Key, Method, play};
use crate::{Action, OutputError, Permission};

/// `kVK_Command`.
const COMMAND: CGKeyCode = 55;
/// `kVK_ANSI_V`.
const V: CGKeyCode = 9;

/// The general pasteboard and the session's event stream.
pub(crate) struct Pasteboard;

/// Whether this process may post keyboard events (the Accessibility
/// permission). When it may not, macOS is asked to show its permission
/// dialog, which returns at once.
pub(crate) fn may_post_events() -> bool {
    CGPreflightPostEventAccess() || CGRequestPostEventAccess()
}

fn string_type() -> &'static NSString {
    // SAFETY: NSPasteboardTypeString is an immutable NSString constant AppKit
    // initialises before any code of ours runs and never changes.
    unsafe { NSPasteboardTypeString }
}

fn unsupported(action: Action) -> OutputError {
    OutputError::Backend {
        action,
        detail: "macOS has no primary selection".to_owned(),
    }
}

impl Backend for Pasteboard {
    fn read_clipboard(&mut self) -> Result<String, OutputError> {
        autoreleasepool(|_| {
            let text = NSPasteboard::generalPasteboard().stringForType(string_type());
            Ok(text.map(|s| s.to_string()).unwrap_or_default())
        })
    }

    fn set_clipboard(&mut self, text: &str) -> Result<(), OutputError> {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            board.clearContents();
            if board.setString_forType(&NSString::from_str(text), string_type()) {
                Ok(())
            } else {
                Err(OutputError::Backend {
                    action: Action::SetClipboard,
                    detail: "NSPasteboard refused the string".to_owned(),
                })
            }
        })
    }

    fn set_primary(&mut self, _text: &str) -> Result<(), OutputError> {
        Err(unsupported(Action::SetPrimary))
    }

    fn release_primary(&mut self) {}

    fn press(&mut self, method: Method) -> Result<(), OutputError> {
        let strokes = match (method, method.strokes()) {
            (Method::CgEvent, Some(strokes)) => strokes,
            _ => {
                return Err(OutputError::Backend {
                    action: Action::Paste,
                    detail: format!("{} is not available on macOS", method.label()),
                });
            }
        };
        // Without the permission CGEventPost drops the events silently.
        if !CGPreflightPostEventAccess() {
            return Err(OutputError::Permission {
                what: Permission::Accessibility,
                detail: "this process may not post keyboard events".to_owned(),
            });
        }
        play(strokes, post)
    }

    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Posts one key edge. Command is the only modifier on the V events, so a
/// modifier the user still holds from the hotkey cannot turn Cmd+V into
/// another shortcut.
fn post(key: Key, down: bool) -> Result<(), OutputError> {
    let (code, flags) = match key {
        Key::Command if down => (COMMAND, CGEventFlags::MaskCommand),
        Key::Command => (COMMAND, CGEventFlags::empty()),
        Key::V => (V, CGEventFlags::MaskCommand),
        Key::Shift | Key::Insert => {
            return Err(OutputError::Backend {
                action: Action::Paste,
                detail: format!("{key:?} has no macOS key code here"),
            });
        }
    };
    let event = CGEvent::new_keyboard_event(None, code, down).ok_or(OutputError::Backend {
        action: Action::Paste,
        detail: "CGEventCreateKeyboardEvent failed".to_owned(),
    })?;
    CGEvent::set_flags(Some(&event), flags);
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
    Ok(())
}
