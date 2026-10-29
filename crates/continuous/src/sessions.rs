//! The session loop of one open capture: with the gate on, one session per
//! utterance; with it off, one long session that reconnects with backoff.

use std::time::Duration;

use anya_core::hub::{LiveSession, Outcome};
use anya_protocol::{EndReason, Source};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::gate::{Feed, Item};
use crate::{BACKOFF_MIN, Settings, Shared, grow};

/// A long session that ran this long was connected: its end resets the backoff.
const STABLE: Duration = Duration::from_secs(10);
/// An engine that takes no audio for this long has stalled; its session is dropped.
const SEND_TIMEOUT: Duration = Duration::from_secs(3);

/// Why streaming into a session stopped.
enum End {
    /// The session ended by itself: final result, error or closed connection.
    Itself(Outcome),
    /// The gate closed: the utterance is over.
    Utterance,
    /// The gate opened for a new utterance while this session ran.
    NextUtterance,
    /// The params changed; a long session reconnects to take them.
    Reconnect,
    /// The engine stopped taking audio.
    Stalled,
    /// Capture is over and everything queued was sent.
    CaptureDown,
}

/// Runs sessions until capture is over and the last session finished.
pub(crate) async fn run(shared: &Shared, feed: &Feed, mut live: watch::Receiver<Settings>) {
    let mut params = shared.params.clone();
    let mut backoff = BACKOFF_MIN;
    // The last session died with audio the engine may not have recognized:
    // the next long session hears it again.
    let mut replay = false;
    // A new utterance's start was already read: open without waiting for one.
    let mut speaking = false;
    loop {
        if !std::mem::take(&mut speaking) && !speech(feed, &mut live).await {
            return;
        }
        params.mark_unchanged();
        let (engine, session_params) = match (shared.choice)() {
            Ok(choice) => choice,
            Err(e) => {
                tracing::warn!(error = %e, ?backoff, "continuous: no engine to open");
                if !pause(feed, backoff).await {
                    return;
                }
                backoff = grow(backoff);
                continue;
            }
        };
        let gated = live.borrow().silence_gate;
        if !gated && !engine.manifest().streaming {
            // Nothing would ever end the session, so its text would never come.
            tracing::error!(
                engine = %engine.manifest().id,
                "continuous: without the silence gate the engine must stream"
            );
            if !park(feed, &mut live, &mut params).await {
                return;
            }
            continue;
        }
        let keep = !gated;
        if std::mem::take(&mut replay) && keep {
            feed.rewind();
        } else {
            feed.forget();
        }
        let opened = Instant::now();
        let mut session = shared
            .hub
            .open(engine.as_ref(), Source::Continuous, session_params);
        let end = stream(feed, &mut session, &live, &mut params, keep).await;
        let final_timeout = live.borrow().final_timeout;
        match end {
            End::Itself(outcome) => {
                if gated && outcome.reason == EndReason::Final {
                    backoff = BACKOFF_MIN;
                    continue;
                }
                if !gated && opened.elapsed() >= STABLE {
                    backoff = BACKOFF_MIN;
                }
                replay = outcome.reason != EndReason::Final;
                tracing::warn!(reason = ?outcome.reason, error = ?outcome.error, ?backoff, "continuous: session ended, reconnecting");
                tracing::debug!(detail = ?outcome.detail, "continuous: session end detail");
            }
            End::Stalled => {
                drop(session);
                replay = keep;
                tracing::warn!(
                    ?backoff,
                    "continuous: engine stopped taking audio, reconnecting"
                );
            }
            End::Utterance => {
                finish(session, final_timeout).await;
                backoff = BACKOFF_MIN;
                continue;
            }
            End::NextUtterance => {
                finish(session, final_timeout).await;
                backoff = BACKOFF_MIN;
                speaking = true;
                continue;
            }
            End::Reconnect => {
                tracing::info!("continuous: session params changed, reconnecting");
                finish(session, final_timeout).await;
                backoff = BACKOFF_MIN;
                continue;
            }
            End::CaptureDown => {
                finish(session, final_timeout).await;
                return;
            }
        }
        if !pause(feed, backoff).await {
            return;
        }
        backoff = grow(backoff);
    }
}

/// Feeds queued audio into `session` until something ends it.
async fn stream(
    feed: &Feed,
    session: &mut LiveSession,
    live: &watch::Receiver<Settings>,
    params: &mut watch::Receiver<u64>,
    keep: bool,
) -> End {
    let mut watching = params.has_changed().is_ok();
    loop {
        tokio::select! {
            biased;
            outcome = session.ended() => return End::Itself(outcome),
            changed = params.changed(), if watching => {
                if changed.is_err() {
                    watching = false;
                } else if !live.borrow().silence_gate {
                    return End::Reconnect;
                }
            }
            item = feed.next(keep) => match item {
                None => return End::CaptureDown,
                Some(Item::End) => return End::Utterance,
                Some(Item::Start) => return End::NextUtterance,
                Some(Item::Audio(chunk)) => {
                    // A session already over refuses the chunk; ended()
                    // reports it on the next turn.
                    if tokio::time::timeout(SEND_TIMEOUT, session.send(chunk)).await.is_err() {
                        return End::Stalled;
                    }
                }
            },
        }
    }
}

/// Finishes `session`, waiting for its final result. Sending the finish waits
/// while the engine is behind; past the deadline the session is dropped and
/// ends closed.
async fn finish(session: LiveSession, final_timeout: Duration) {
    match tokio::time::timeout(final_timeout + SEND_TIMEOUT, session.finish(final_timeout)).await {
        Ok(outcome) if outcome.reason != EndReason::Final => {
            tracing::debug!(reason = ?outcome.reason, error = ?outcome.error, "continuous: session finished without a final result");
        }
        Ok(_) => {}
        Err(_) => tracing::warn!("continuous: engine took no finish, session dropped"),
    }
}

/// Waits until a session should open: at once without the gate, at the next
/// utterance with it. Audio from before the utterance is dropped. False once
/// capture is over.
async fn speech(feed: &Feed, live: &mut watch::Receiver<Settings>) -> bool {
    loop {
        if !live.borrow_and_update().silence_gate {
            return !feed.is_closed();
        }
        tokio::select! {
            item = feed.next(false) => match item {
                Some(Item::Start) => return true,
                Some(_) => {}
                None => return false,
            },
            changed = live.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
        }
    }
}

/// Waits `backoff`. False once capture is over.
async fn pause(feed: &Feed, backoff: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(backoff) => true,
        () = feed.closed() => false,
    }
}

/// Waits for the settings or the params to change. False once capture is over.
async fn park(
    feed: &Feed,
    live: &mut watch::Receiver<Settings>,
    params: &mut watch::Receiver<u64>,
) -> bool {
    let watching = params.has_changed().is_ok();
    tokio::select! {
        () = feed.closed() => false,
        changed = live.changed() => changed.is_ok(),
        _ = params.changed(), if watching => true,
    }
}
