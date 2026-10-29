//! Dictation: when a hotkey opens and ends a session, and what happens to its text.
//!
//! [`spawn`] starts one task that owns the microphone and the sessions it
//! records. The app maps hotkey edges to [`Input`]s and hands them over with
//! [`Dictation::input`], which only queues them, so a hotkey callback returns
//! at once. Inputs are handled in the order they were given, however fast
//! they come.
//!
//! - **Toggle**: a press starts recording; the next press stops it after the
//!   stop delay; a press during the stop delay stops at once and starts a new
//!   recording. Releases are ignored.
//! - **Hold**: a press starts recording, or resumes one in its stop delay;
//!   the stop delay begins once every held dictation hotkey is released.
//!
//! Stopping releases the device first, then waits for the engine's final
//! result in the background, so the next recording can start right away.
//! Each session the user stopped yields one [`Finished`] with its text, unless
//! it recognized nothing or was cancelled; a session that timed out or failed
//! while finishing still yields what it recognized, and the failure shows. A
//! session the engine ends with a final result while recording yields its
//! text too; one that fails while recording yields nothing. Capture failing
//! shows [`anya_protocol::ErrorCode::Audio`] and abandons the recording, or
//! finishes it when it was in its stop delay. [`Input::Cancel`] throws away the
//! recording, every session still waiting for its result, and a failure
//! being shown; [`Input::Retry`] starts a new recording while a failure is
//! shown. A failure shows for [`ERROR_HOLD`], then the state returns to idle.

mod controller;

use std::sync::Arc;
use std::time::Duration;

use anya_core::audio::AudioSource;
use anya_core::engine::Choice;
use anya_core::hub::Hub;
use anya_protocol::{ErrorCode, SessionId};
use tokio::sync::{mpsc, watch};

use crate::controller::Command;

/// How long a stopped session waits for the engine's final result by default.
pub const FINAL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a failure stays shown before the state returns to idle.
pub const ERROR_HOLD: Duration = Duration::from_secs(10);

/// How dictation hotkeys start and stop a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// A press starts recording, the next press stops it.
    #[default]
    Toggle,
    /// Recording lasts while a dictation hotkey is held.
    Hold,
}

/// What dictation reads from the config. A change applies from the next
/// input: the device and engine from the next recording, the stop delay and
/// final timeout from the next stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Toggle or hold.
    pub mode: Mode,
    /// How long recording goes on after the user asked it to stop, so the
    /// last word is not cut off. Zero stops at once.
    pub stop_delay: Duration,
    /// How long a stopped session waits for the engine's final result.
    pub final_timeout: Duration,
    /// The input device's id; `None` is the system default.
    pub device: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            mode: Mode::Toggle,
            stop_delay: Duration::ZERO,
            final_timeout: FINAL_TIMEOUT,
            device: None,
        }
    }
}

/// What the user did. The app maps key edges to these, in both modes: every
/// `Press` of a hotkey is followed by its `Release`, and a `Press` of a key
/// already down is a key repeat and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A dictation hotkey went down; the number tells hotkeys apart.
    Press(u32),
    /// A dictation hotkey went up.
    Release(u32),
    /// Throw away the recording, the sessions waiting for their result and
    /// the failure being shown (Esc).
    Cancel,
    /// Start a new recording while a failure is shown (R).
    Retry,
}

/// What dictation is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum State {
    /// Waiting for a hotkey.
    #[default]
    Idle,
    /// The microphone is open and audio goes to the engine.
    Recording,
    /// Still recording: the stop delay runs.
    Stopping,
    /// The last recording or session failed. Shown for [`ERROR_HOLD`] unless
    /// a new recording starts or the user cancels.
    Failed {
        /// What went wrong; the app translates it.
        error: ErrorCode,
        /// The engine's or the system's own words, untranslated.
        detail: Option<String>,
    },
}

/// Dictation's state, published on every change.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// The recording's state, or the failure shown.
    pub state: State,
    /// Stopped sessions waiting for their final result in the background.
    pub finishing: usize,
}

/// The text of a session the user stopped, for the output sink. Sent once
/// per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// The session it came from.
    pub session: SessionId,
    /// The session's whole text.
    pub text: String,
}

/// Hands inputs and settings to the running dictation. Cheap to clone; when
/// every clone is dropped, dictation stops: a recording is abandoned and
/// sessions still finishing end without a [`Finished`].
#[derive(Debug, Clone)]
pub struct Dictation {
    commands: mpsc::UnboundedSender<Command>,
}

impl Dictation {
    /// Queues an input and returns at once; safe on a hotkey thread.
    pub fn input(&self, input: Input) {
        let _ = self.commands.send(Command::Input(input));
    }

    /// Replaces the settings. A changed mode forgets which hotkeys are held.
    pub fn configure(&self, settings: Settings) {
        let _ = self.commands.send(Command::Configure(settings));
    }
}

/// Starts dictation on the current Tokio runtime, with the dictation source
/// idle. Returns the handle, the status, and the text of every session the
/// user stopped.
pub fn spawn(
    hub: Hub,
    audio: Arc<dyn AudioSource>,
    choice: Choice,
    settings: Settings,
) -> (
    Dictation,
    watch::Receiver<Status>,
    mpsc::UnboundedReceiver<Finished>,
) {
    let (commands, status, finished) = controller::spawn(hub, audio, choice, settings);
    (Dictation { commands }, status, finished)
}
