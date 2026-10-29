//! What readers of the hub see, with scripted engines in place of real ones.

use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{Engine, EngineError, Input, Output, Session, SessionParams};
use anya_core::hub::{Hub, HubInfo, Outcome, Subscription};
use anya_protocol::{
    EndReason, ErrorCode, Frame, Manifest, Recognition, Source, SourceState, Utterance, contract,
};
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
enum Script {
    /// Ready, a partial per chunk, the final on Finish.
    Echo,
    /// Ready, then never answers.
    Mute,
    /// Cannot connect.
    Unreachable,
    /// Ready, one partial, then closes.
    HangUp,
}

struct Fake {
    manifest: Manifest,
    script: Script,
}

fn fake(script: Script) -> Fake {
    Fake {
        manifest: Manifest {
            id: "fake".to_owned(),
            title: "Fake".into(),
            blurb: Default::default(),
            docs: None,
            streaming: true,
            fields: Vec::new(),
        },
        script,
    }
}

fn partial(text: String, is_final: bool) -> Recognition {
    Recognition {
        utterances: vec![Utterance {
            text: text.clone(),
            definite: is_final,
        }],
        text,
        is_final,
    }
}

impl Engine for Fake {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, _: SessionParams) -> Session {
        let (input, mut audio) = mpsc::channel(64);
        let (tx, output) = mpsc::channel(64);
        let script = self.script;
        tokio::spawn(async move {
            if let Script::Unreachable = script {
                let _ = tx
                    .send(Output::Error(EngineError::Connect("refused".into())))
                    .await;
                return;
            }
            let _ = tx.send(Output::Ready).await;
            let mut text = String::new();
            while let Some(input) = audio.recv().await {
                match (script, input) {
                    (Script::Mute, _) => {}
                    (_, Input::Audio(_)) => {
                        text.push('a');
                        let _ = tx.send(Output::Result(partial(text.clone(), false))).await;
                        if let Script::HangUp = script {
                            return;
                        }
                    }
                    (_, Input::Finish) => {
                        let _ = tx
                            .send(Output::Result(partial(text.clone() + ".", true)))
                            .await;
                        return;
                    }
                }
            }
            // Mute keeps its end open until the session is dropped.
            std::future::pending::<()>().await;
        });
        Session { input, output }
    }
}

fn hub() -> Hub {
    Hub::new(HubInfo {
        version: "0.1.0".to_owned(),
        session_prefix: "anya-test".to_owned(),
    })
}

fn loud() -> Chunk {
    Chunk([0x00, 0x40].repeat(320))
}

/// The snapshot and every frame published since.
fn read(sub: &mut Subscription) -> Vec<Frame> {
    let mut frames = std::mem::take(&mut sub.snapshot);
    while let Ok(f) = sub.frames.try_recv() {
        frames.push(f);
    }
    frames
}

async fn started(sub: &mut Subscription) {
    loop {
        let frame = sub.frames.recv().await.expect("frames");
        sub.snapshot.push(frame.clone());
        if matches!(frame, Frame::SessionStart { .. }) {
            return;
        }
    }
}

#[tokio::test]
async fn a_dictated_sentence_reads_as_promised() {
    let hub = hub();
    let mut sub = hub.subscribe();
    hub.set_source(Source::Dictation, SourceState::Listening, None);
    let engine = fake(Script::Echo);
    let session = hub.open(&engine, Source::Dictation, SessionParams::default());
    started(&mut sub).await;
    assert!(session.send(loud()).await);
    assert!(session.send(loud()).await);
    let outcome = session.finish(Duration::from_secs(5)).await;
    assert_eq!(
        (outcome.reason, outcome.text.as_str()),
        (EndReason::Final, "aa.")
    );

    let frames = read(&mut sub);
    assert_eq!(contract::check(&frames), Ok(()));
    let levels = frames
        .iter()
        .filter(|f| matches!(f, Frame::Level { level, .. } if *level > 0.5));
    assert_eq!(levels.count(), 2);
    assert!(matches!(
        frames.last(),
        Some(Frame::SessionEnd {
            reason: EndReason::Final,
            error: None,
            ..
        })
    ));
}

#[tokio::test]
async fn a_late_reader_catches_up_from_the_snapshot() {
    let hub = hub();
    let mut first = hub.subscribe();
    hub.set_source(
        Source::Continuous,
        SourceState::Listening,
        Some("Loop".into()),
    );
    let engine = fake(Script::Echo);
    let session = hub.open(&engine, Source::Continuous, SessionParams::default());
    started(&mut first).await;
    session.send(loud()).await;
    // The partial is published before the reader joins.
    while !read(&mut first)
        .iter()
        .any(|f| matches!(f, Frame::Result { .. }))
    {
        tokio::task::yield_now().await;
    }

    let mut late = hub.subscribe();
    let kinds: Vec<&str> = late
        .snapshot
        .iter()
        .map(|f| match f {
            Frame::Hello { .. } => "hello",
            Frame::Source { .. } => "source",
            Frame::SessionStart { .. } => "session_start",
            Frame::Result { .. } => "result",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["hello", "source", "session_start", "result"]);

    session.send(loud()).await;
    session.finish(Duration::from_secs(5)).await;
    assert_eq!(contract::check(&read(&mut late)), Ok(()));
}

#[tokio::test]
async fn cancelling_keeps_what_was_heard_but_says_cancelled() {
    let hub = hub();
    let mut sub = hub.subscribe();
    let engine = fake(Script::Echo);
    let session = hub.open(&engine, Source::Dictation, SessionParams::default());
    started(&mut sub).await;
    session.send(loud()).await;
    while !sub
        .snapshot
        .iter()
        .any(|f| matches!(f, Frame::Result { .. }))
    {
        sub.snapshot.push(sub.frames.recv().await.expect("frames"));
    }
    let outcome = session.cancel().await;
    assert_eq!(
        (outcome.reason, outcome.text.as_str()),
        (EndReason::Cancelled, "a")
    );
    let frames = read(&mut sub);
    assert_eq!(contract::check(&frames), Ok(()));
    assert!(matches!(
        frames.last(),
        Some(Frame::SessionEnd {
            reason: EndReason::Cancelled,
            ..
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn a_final_that_never_comes_times_out_as_an_error() {
    let hub = hub();
    let mut sub = hub.subscribe();
    let engine = fake(Script::Mute);
    let session = hub.open(&engine, Source::Dictation, SessionParams::default());
    started(&mut sub).await;
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(
        (outcome.reason, outcome.error),
        (EndReason::Error, Some(ErrorCode::Timeout))
    );
    let frames = read(&mut sub);
    assert_eq!(contract::check(&frames), Ok(()));
    assert!(matches!(
        frames.last(),
        Some(Frame::SessionEnd {
            error: Some(ErrorCode::Timeout),
            ..
        })
    ));
}

#[tokio::test]
async fn an_unreachable_engine_publishes_no_session() {
    let hub = hub();
    let mut sub = hub.subscribe();
    let engine = fake(Script::Unreachable);
    let mut session = hub.open(&engine, Source::Dictation, SessionParams::default());
    let outcome = session.ended().await;
    assert_eq!(
        outcome,
        Outcome {
            reason: EndReason::Error,
            text: String::new(),
            error: Some(ErrorCode::Connect),
            detail: Some("refused".into()),
        }
    );
    let frames = read(&mut sub);
    assert_eq!(frames.len(), 1, "only hello: {frames:?}");
}

#[tokio::test]
async fn an_engine_that_hangs_up_ends_closed_with_its_text() {
    let hub = hub();
    let mut sub = hub.subscribe();
    let engine = fake(Script::HangUp);
    let mut session = hub.open(&engine, Source::Continuous, SessionParams::default());
    started(&mut sub).await;
    session.send(loud()).await;
    let outcome = session.ended().await;
    assert_eq!(
        (outcome.reason, outcome.text.as_str()),
        (EndReason::Closed, "a")
    );
    assert_eq!(contract::check(&read(&mut sub)), Ok(()));
}

#[tokio::test]
async fn a_source_frame_is_sent_only_on_change() {
    let hub = hub();
    let mut sub = hub.subscribe();
    hub.set_source(
        Source::Continuous,
        SourceState::Listening,
        Some("Loop".into()),
    );
    hub.set_source(
        Source::Continuous,
        SourceState::Listening,
        Some("Loop".into()),
    );
    hub.set_source(Source::Continuous, SourceState::Idle, Some("Loop".into()));
    let sources = read(&mut sub)
        .into_iter()
        .filter(|f| matches!(f, Frame::Source { .. }));
    assert_eq!(sources.count(), 2);
    assert_eq!(
        hub.subscribe().snapshot.len(),
        2,
        "hello and the latest source state"
    );
}
