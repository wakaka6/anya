//! One plugin's side of its Bun process: the writer to its stdin, the
//! sessions open on it and the hook calls waiting for an answer. A link
//! outlives the process: when the process dies every session and call on it
//! fails, and the next process fills the link again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use anya_core::engine::{EngineError, Output};
use anya_core::hooks::{Extra, HookError};
use anya_protocol::HostCommand;
use tokio::sync::{mpsc, oneshot};

/// Commands waiting for a process's stdin: 40 ms of audio each, so about
/// forty seconds of one session. A process that falls further behind than
/// that ends the sessions that cannot be served.
pub(crate) const COMMAND_QUEUE: usize = 1024;

/// Results queued per session for the hub, which drains them at once.
pub(crate) const OUTPUT_QUEUE: usize = 64;

/// What a plugin answered a hook call with.
pub(crate) enum Answer {
    Context(Extra),
    Text(String),
    Done,
}

/// The end of a hook call.
pub(crate) type CallResult = Result<Answer, HookError>;

/// A call waiting for its answer: the id, the writer to cancel it on and
/// where the answer arrives.
pub(crate) struct Call {
    pub(crate) id: String,
    pub(crate) writer: mpsc::Sender<HostCommand>,
    pub(crate) answer: oneshot::Receiver<CallResult>,
}

#[derive(Default)]
struct State {
    /// Where commands go while the process runs.
    writer: Option<mpsc::Sender<HostCommand>>,
    /// Open sessions by session id.
    sessions: HashMap<String, mpsc::Sender<Output>>,
    /// Hook calls by call id.
    calls: HashMap<String, oneshot::Sender<CallResult>>,
}

/// What a plugin's engine, hooks, supervisor and reader share.
pub(crate) struct Link {
    plugin: String,
    state: Mutex<State>,
    next: AtomicU64,
}

impl Link {
    pub(crate) fn new(plugin: String) -> Link {
        Link {
            plugin,
            state: Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    /// The plugin id.
    pub(crate) fn plugin(&self) -> &str {
        &self.plugin
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-update; the state stays usable.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn id(&self, prefix: char) -> String {
        format!("{prefix}{}", self.next.fetch_add(1, Ordering::Relaxed))
    }

    /// The process is up and takes commands.
    pub(crate) fn set_writer(&self, writer: mpsc::Sender<HostCommand>) {
        self.state().writer = Some(writer);
    }

    /// Whether a process is up and takes commands.
    pub(crate) fn is_up(&self) -> bool {
        self.state().writer.is_some()
    }

    /// Registers a session and returns its id and the writer to the
    /// process, or `None` while no process runs.
    pub(crate) fn register(
        &self,
        output: mpsc::Sender<Output>,
    ) -> Option<(String, mpsc::Sender<HostCommand>)> {
        let mut state = self.state();
        let writer = state.writer.clone()?;
        let id = self.id('s');
        state.sessions.insert(id.clone(), output);
        Some((id, writer))
    }

    /// Forgets a session; true when it was still open.
    pub(crate) fn take_session(&self, id: &str) -> bool {
        self.state().sessions.remove(id).is_some()
    }

    /// Ends a session from this side with `error`.
    pub(crate) fn fail_session(&self, id: &str, error: EngineError) {
        if let Some(output) = self.state().sessions.remove(id) {
            let _ = output.try_send(Output::Error(error));
        }
    }

    /// Delivers `output` to the session; `last` forgets the session after
    /// it. Returns false, and forgets the session, when its reader is gone
    /// or too slow.
    pub(crate) fn deliver(&self, session: &str, output: Output, last: bool) -> Option<bool> {
        let mut state = self.state();
        let tx = state.sessions.get(session)?;
        let delivered = tx.try_send(output).is_ok();
        if last || !delivered {
            state.sessions.remove(session);
        }
        Some(delivered)
    }

    /// Starts a hook call: its id, the writer and the receiver for the
    /// answer, or `None` while no process runs.
    pub(crate) fn begin_call(&self) -> Option<Call> {
        let mut state = self.state();
        let writer = state.writer.clone()?;
        let id = self.id('c');
        let (tx, answer) = oneshot::channel();
        state.calls.insert(id.clone(), tx);
        Some(Call { id, writer, answer })
    }

    /// Hands a call its answer. A call nobody waits for any more (budget
    /// spent, cancelled) drops it.
    pub(crate) fn finish_call(&self, id: &str, result: CallResult) {
        if let Some(call) = self.state().calls.remove(id) {
            let _ = call.send(result);
        }
    }

    /// Stops waiting for a call.
    pub(crate) fn forget_call(&self, id: &str) {
        self.state().calls.remove(id);
    }

    /// The process is gone: every session ends with an error and every call
    /// fails as unavailable, both with `reason`.
    pub(crate) fn fail_all(&self, reason: &str) {
        let (sessions, calls) = {
            let mut state = self.state();
            state.writer = None;
            (
                std::mem::take(&mut state.sessions),
                std::mem::take(&mut state.calls),
            )
        };
        for (_, output) in sessions {
            let _ = output.try_send(Output::Error(EngineError::Upstream(reason.to_owned())));
        }
        for (_, call) in calls {
            let _ = call.send(Err(HookError::Unavailable(reason.to_owned())));
        }
    }
}
