//! Linux: the clipboard through `xclip` (X11) or `wl-clipboard` (Wayland),
//! the paste keys through XTest, `wtype` or `/dev/uinput`.

mod run;
mod uinput;
mod x11;

use std::time::Duration;

use anya_core::desktop::Desktop;

use crate::plan::{Backend, Method, cmd};
use crate::{Action, OutputError};
use run::{DEADLINE, Owner, RunError};

/// The tools of one Linux desktop, and the primary selection owner while a
/// paste holds it.
pub(crate) struct Tools {
    desktop: Desktop,
    primary: Option<Owner>,
}

impl Tools {
    pub(crate) fn new(desktop: Desktop) -> Tools {
        Tools {
            desktop,
            primary: None,
        }
    }
}

/// `argv` failing at `action`, a missing executable named after `argv[0]`.
fn error(argv: &[&'static str], action: Action, e: RunError) -> OutputError {
    match e {
        RunError::NotFound => OutputError::MissingTool {
            tool: argv.first().copied().unwrap_or_default(),
        },
        RunError::Failed(detail) => OutputError::Backend { action, detail },
    }
}

fn pipe(argv: &[&'static str], text: &str, action: Action) -> Result<(), OutputError> {
    run::run(argv, Some(text), false, DEADLINE)
        .map(drop)
        .map_err(|e| error(argv, action, e))
}

impl Backend for Tools {
    fn read_clipboard(&mut self) -> Result<String, OutputError> {
        let argv = match self.desktop {
            Desktop::Wayland => cmd::WL_PASTE,
            _ => cmd::XCLIP_READ,
        };
        run::run(argv, None, true, DEADLINE).map_err(|e| error(argv, Action::ReadClipboard, e))
    }

    fn set_clipboard(&mut self, text: &str) -> Result<(), OutputError> {
        let argv = match self.desktop {
            Desktop::Wayland => cmd::WL_COPY,
            _ => cmd::XCLIP_WRITE,
        };
        pipe(argv, text, Action::SetClipboard)
    }

    fn set_primary(&mut self, text: &str) -> Result<(), OutputError> {
        match self.desktop {
            // wl-copy serves the selection until another client takes it.
            Desktop::Wayland => pipe(cmd::WL_COPY_PRIMARY, text, Action::SetPrimary),
            _ => {
                let owner = Owner::start(cmd::XCLIP_PRIMARY, text)
                    .map_err(|e| error(cmd::XCLIP_PRIMARY, Action::SetPrimary, e))?;
                self.primary = Some(owner);
                Ok(())
            }
        }
    }

    fn release_primary(&mut self) {
        self.primary = None;
    }

    fn press(&mut self, method: Method) -> Result<(), OutputError> {
        let strokes = method.strokes().unwrap_or_default();
        match method {
            Method::XTest => x11::press(strokes),
            Method::Uinput => uinput::press(strokes),
            Method::Wtype => run::run(cmd::WTYPE, None, false, DEADLINE)
                .map(drop)
                .map_err(|e| error(cmd::WTYPE, Action::Paste, e)),
            Method::CgEvent => Err(OutputError::Backend {
                action: Action::Paste,
                detail: format!("{} is not available on Linux", method.label()),
            }),
        }
    }

    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_executable_names_the_tool() {
        assert_eq!(
            error(cmd::WTYPE, Action::Paste, RunError::NotFound),
            OutputError::MissingTool { tool: "wtype" }
        );
        assert_eq!(
            error(
                cmd::XCLIP_WRITE,
                Action::SetClipboard,
                RunError::Failed("Error: Can't open display".into())
            ),
            OutputError::Backend {
                action: Action::SetClipboard,
                detail: "Error: Can't open display".into()
            }
        );
    }
}
