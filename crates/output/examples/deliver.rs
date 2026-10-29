//! Delivers a string on this desktop and reads the clipboard back.
//!
//! ```text
//! cargo run -p anya-output --example deliver -- "text"          # clipboard
//! cargo run -p anya-output --example deliver -- --paste "text"  # paste after 3 s
//! ```

use std::io::{self, Write};
use std::process::{Command, ExitCode};
use std::time::Duration;

use anya_core::desktop::Desktop;
use anya_output::{Delivery, checks, deliver, missing_tools};

/// The command that prints the clipboard on `desktop`.
fn reader(desktop: Desktop) -> &'static [&'static str] {
    match desktop {
        Desktop::Darwin => &["pbpaste"],
        Desktop::Wayland => &["wl-paste", "--no-newline"],
        Desktop::X11 => &["xclip", "-o", "-selection", "clipboard"],
    }
}

fn main() -> ExitCode {
    let mut how = Delivery::Clipboard;
    let mut text = String::from("Anya 你好, clipboard check");
    for arg in std::env::args().skip(1) {
        if arg == "--paste" {
            how = Delivery::Paste;
        } else {
            text = arg;
        }
    }
    let desktop = Desktop::detect();
    let mut out = io::stdout();
    let _ = writeln!(out, "desktop: {desktop}");
    let _ = writeln!(out, "missing tools: {:?}", missing_tools(desktop));
    let _ = writeln!(out, "checks: {:?}", checks(desktop));
    if how == Delivery::Paste {
        let _ = writeln!(out, "focus a text field; pasting in 3 s");
        std::thread::sleep(Duration::from_secs(3));
    }
    let result = deliver(desktop, &text, how);
    let _ = writeln!(out, "deliver({how:?}): {result:?}");

    let argv = reader(desktop);
    let back = Command::new(argv[0]).args(&argv[1..]).output();
    let back = match back {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => {
            let _ = writeln!(out, "reading the clipboard with {}: {e}", argv[0]);
            return ExitCode::FAILURE;
        }
    };
    let _ = writeln!(out, "clipboard now: {back:?}");
    let _ = writeln!(out, "matches: {}", back == text);
    if result.is_ok() && back == text {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
