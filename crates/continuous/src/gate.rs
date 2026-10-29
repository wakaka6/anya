//! The silence gate, and the queue between capture and the session loop.
//!
//! Capture never waits on an engine: every chunk goes through the gate into a
//! bounded queue at once, and the session loop takes it from there. The queue
//! keeps the last few seconds, so audio captured while a session connects or
//! reconnects reaches the engine late rather than not at all.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anya_core::audio::{Chunk, SAMPLE_RATE};
use tokio::sync::Notify;

/// One chunk this loud opens the gate: about -44 dBFS.
const ONSET: f32 = level_at(-44.0);
/// While the gate is open, a chunk quieter than this counts as silence: about
/// -47 dBFS. Below the onset, so a voice trailing off does not flap the gate.
const RELEASE: f32 = level_at(-47.0);
/// Silence this long ends an utterance. The silent chunks inside it are still
/// sent, so the tail of a sentence is not cut. Engines time out on silence far
/// later, so the gate always ends the session first.
const HANGOVER: Duration = Duration::from_millis(1200);
/// Audio kept while the gate is shut, so the syllable that opened it is not
/// the first thing the engine hears.
const PREROLL_BYTES: usize = bytes_for(600);
/// Audio the queue holds: enough to cover a connect, and what a reconnecting
/// session without the gate replays.
const REPLAY_BYTES: usize = bytes_for(5000);

const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;

const fn bytes_for(ms: usize) -> usize {
    BYTES_PER_SECOND * ms / 1000
}

/// A dBFS value on [`Chunk::level`]'s scale, where -60 dBFS is 0.0 and full
/// scale is 1.0.
const fn level_at(dbfs: f32) -> f32 {
    (dbfs + 60.0) / 60.0
}

/// How long `len` bytes of captured audio last. Measured, never assumed: an
/// adapter may hand over short chunks.
fn duration_of(len: usize) -> Duration {
    Duration::from_micros(len as u64 * 1_000_000 / BYTES_PER_SECOND as u64)
}

/// What the session loop reads.
#[derive(Clone)]
pub(crate) enum Item {
    /// The gate opened: an utterance starts.
    Start,
    /// Captured audio.
    Audio(Chunk),
    /// The gate closed: the utterance is over.
    End,
}

/// Items in capture order. Those before `sent` were already handed to a
/// session and are kept only to replay.
#[derive(Default)]
struct Queue {
    items: VecDeque<Item>,
    sent: usize,
    bytes: usize,
    closed: bool,
}

impl Queue {
    fn push(&mut self, item: Item) {
        if let Item::Audio(c) = &item {
            self.bytes += c.0.len();
        }
        self.items.push_back(item);
        // Drop the oldest audio, sent audio first, so the newest always gets
        // through. Markers stay: they keep one utterance from running into
        // the next.
        while self.bytes > REPLAY_BYTES {
            let Some(i) = self
                .items
                .iter()
                .position(|it| matches!(it, Item::Audio(_)))
            else {
                break;
            };
            if i + 1 == self.items.len() {
                break;
            }
            if let Some(Item::Audio(c)) = self.items.remove(i) {
                self.bytes -= c.0.len();
            }
            if i < self.sent {
                self.sent -= 1;
            }
        }
    }

    /// The next unsent item. With `keep`, it stays in the queue for a replay.
    fn take(&mut self, keep: bool) -> Option<Item> {
        if !keep {
            self.forget();
            let item = self.items.pop_front()?;
            if let Item::Audio(c) = &item {
                self.bytes -= c.0.len();
            }
            return Some(item);
        }
        let item = self.items.get(self.sent)?.clone();
        self.sent += 1;
        Some(item)
    }

    fn forget(&mut self) {
        for item in self.items.drain(..self.sent) {
            if let Item::Audio(c) = item {
                self.bytes -= c.0.len();
            }
        }
        self.sent = 0;
    }

    fn rewind(&mut self) {
        let mut i = 0;
        while i < self.sent {
            if matches!(self.items[i], Item::Audio(_)) {
                i += 1;
            } else {
                self.items.remove(i);
                self.sent -= 1;
            }
        }
        self.sent = 0;
    }
}

/// Where the gate is between chunks.
#[derive(Default)]
struct Gate {
    open: bool,
    silent: Duration,
    preroll: VecDeque<Chunk>,
    preroll_bytes: usize,
}

impl Gate {
    fn push(&mut self, enabled: bool, chunk: Chunk, out: &mut Queue) {
        if !enabled {
            // Everything streams; audio held for an onset goes first. An
            // utterance in progress ends here, so its session finishes and
            // the next one opens as a long session.
            if self.open {
                out.push(Item::End);
            }
            self.open = false;
            self.silent = Duration::ZERO;
            self.preroll_bytes = 0;
            for held in self.preroll.drain(..) {
                out.push(Item::Audio(held));
            }
            out.push(Item::Audio(chunk));
            return;
        }
        let level = chunk.level();
        if self.open {
            if level >= RELEASE {
                self.silent = Duration::ZERO;
            } else {
                self.silent += duration_of(chunk.0.len());
            }
            let ended = self.silent >= HANGOVER;
            out.push(Item::Audio(chunk));
            if ended {
                tracing::debug!(level, "continuous: utterance ended");
                self.open = false;
                self.silent = Duration::ZERO;
                out.push(Item::End);
            }
            return;
        }
        if level < ONSET {
            self.preroll_bytes += chunk.0.len();
            self.preroll.push_back(chunk);
            while self.preroll_bytes > PREROLL_BYTES && self.preroll.len() > 1 {
                if let Some(old) = self.preroll.pop_front() {
                    self.preroll_bytes -= old.0.len();
                }
            }
            return;
        }
        tracing::debug!(level, "continuous: speech started");
        self.open = true;
        self.silent = Duration::ZERO;
        self.preroll_bytes = 0;
        out.push(Item::Start);
        for held in self.preroll.drain(..) {
            out.push(Item::Audio(held));
        }
        out.push(Item::Audio(chunk));
    }
}

#[derive(Default)]
struct State {
    gate: Gate,
    queue: Queue,
}

/// The gate and the queue of one open capture: capture pushes, the session
/// loop reads.
#[derive(Default)]
pub(crate) struct Feed {
    state: Mutex<State>,
    notify: Notify,
}

impl Feed {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock; a poisoned one is still whole.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Hands a captured chunk to the gate, `gated` saying whether it is on.
    pub(crate) fn push(&self, gated: bool, chunk: Chunk) {
        {
            let mut state = self.lock();
            let State { gate, queue } = &mut *state;
            gate.push(gated, chunk, queue);
        }
        self.notify.notify_one();
    }

    /// Capture is over: readers get what is queued, then `None`.
    pub(crate) fn close(&self) {
        self.lock().queue.closed = true;
        self.notify.notify_one();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().queue.closed
    }

    /// The next item, waiting for one; `None` once capture is over and the
    /// queue is empty. With `keep`, audio stays queued for [`Feed::rewind`].
    /// Cancel-safe: an item is taken only when it is returned.
    pub(crate) async fn next(&self, keep: bool) -> Option<Item> {
        loop {
            let notified = self.notify.notified();
            {
                let mut state = self.lock();
                if let Some(item) = state.queue.take(keep) {
                    return Some(item);
                }
                if state.queue.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Resolves once capture is over.
    pub(crate) async fn closed(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_closed() {
                return;
            }
            notified.await;
        }
    }

    /// Reads the kept audio again, from the oldest still queued.
    pub(crate) fn rewind(&self) {
        self.lock().queue.rewind();
    }

    /// Drops the kept audio: it reached a session that ended cleanly.
    pub(crate) fn forget(&self) {
        self.lock().queue.forget();
    }
}
