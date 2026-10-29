//! Prints the edges of global hotkeys on this desktop:
//!
//! ```text
//! cargo run -p anya-hotkey --example listen -- [--seconds N] [--record] [COMBO...]
//! ```
//!
//! Without combos it registers `Alt+Super` and `F13`. `--record` captures
//! one combo instead, the way a settings page does.

use std::io::Write;
use std::time::Duration;

use anya_core::desktop::Desktop;
use anya_hotkey::{Combo, Hotkeys, format_list};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut seconds = 30;
    let mut record = false;
    let mut combos: Vec<Combo> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seconds" => seconds = args.next().ok_or("--seconds needs a number")?.parse()?,
            "--record" => record = true,
            combo => combos.push(combo.parse()?),
        }
    }
    if combos.is_empty() {
        combos = vec!["Alt+Super".parse()?, "F13".parse()?];
    }

    let desktop = Desktop::detect();
    let (hotkeys, mut events) = Hotkeys::start(desktop)?;
    let mut out = std::io::stdout();
    writeln!(out, "listening on {desktop} with {}", hotkeys.backend())?;
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);

    if record {
        writeln!(out, "press a combo (Esc cancels)")?;
        tokio::select! {
            combo = hotkeys.record() => match combo {
                Some(combo) => writeln!(out, "recorded {combo}")?,
                None => writeln!(out, "cancelled")?,
            },
            () = &mut deadline => writeln!(out, "timed out")?,
        }
        return Ok(());
    }

    if let Err(unsupported) = hotkeys.set(&combos) {
        writeln!(out, "{unsupported}")?;
    }
    writeln!(out, "registered {}", format_list(&combos))?;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => writeln!(out, "{:?} {}", event.edge, event.combo)?,
                None => {
                    writeln!(out, "the backend stopped")?;
                    break;
                }
            },
            () = &mut deadline => break,
        }
    }
    Ok(())
}
