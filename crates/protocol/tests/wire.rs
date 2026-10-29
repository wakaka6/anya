//! The JSON every subscriber sees. Changing an expectation here is a protocol
//! change: additive while PROTOCOL_VERSION stays, a version bump otherwise.

use anya_protocol::{
    EndReason, Frame, PROTOCOL_VERSION, Recognition, SessionId, Source, SourceState, Utterance,
};
use serde_json::{Value, json};

fn session() -> SessionId {
    SessionId("anya-20261030T101500-1".to_owned())
}

#[track_caller]
fn pinned(frame: Frame, wire: Value) {
    assert_eq!(serde_json::to_value(&frame).expect("encode"), wire);
    let read: Frame = serde_json::from_value(wire).expect("decode");
    assert_eq!(read, frame);
}

#[test]
fn hello() {
    pinned(
        Frame::Hello {
            name: "anya".to_owned(),
            version: "0.1.0".to_owned(),
            protocol: PROTOCOL_VERSION,
        },
        json!({"type": "hello", "name": "anya", "version": "0.1.0", "protocol": 0}),
    );
}

#[test]
fn source_names_its_device_only_when_known() {
    pinned(
        Frame::Source {
            source: Source::Continuous,
            state: SourceState::Listening,
            device: Some("BlackHole 2ch".to_owned()),
        },
        json!({"type": "source", "source": "continuous", "state": "listening", "device": "BlackHole 2ch"}),
    );
    pinned(
        Frame::Source {
            source: Source::Dictation,
            state: SourceState::Idle,
            device: None,
        },
        json!({"type": "source", "source": "dictation", "state": "idle"}),
    );
}

#[test]
fn session_start() {
    pinned(
        Frame::SessionStart {
            session: session(),
            source: Source::Dictation,
            engine: "example".to_owned(),
        },
        json!({"type": "session_start", "session": "anya-20261030T101500-1", "source": "dictation", "engine": "example"}),
    );
}

#[test]
fn result_carries_the_recognition_inline() {
    pinned(
        Frame::Result {
            session: session(),
            recognition: Recognition {
                text: "你好。我们".to_owned(),
                utterances: vec![
                    Utterance {
                        text: "你好。".to_owned(),
                        definite: true,
                    },
                    Utterance {
                        text: "我们".to_owned(),
                        definite: false,
                    },
                ],
                is_final: false,
            },
        },
        json!({
            "type": "result",
            "session": "anya-20261030T101500-1",
            "text": "你好。我们",
            "utterances": [{"text": "你好。", "definite": true}, {"text": "我们", "definite": false}],
            "final": false
        }),
    );
}

#[test]
fn session_end_reasons() {
    for (reason, word) in [
        (EndReason::Final, "final"),
        (EndReason::Error, "error"),
        (EndReason::Closed, "closed"),
        (EndReason::Cancelled, "cancelled"),
    ] {
        pinned(
            Frame::SessionEnd {
                session: session(),
                reason,
            },
            json!({"type": "session_end", "session": "anya-20261030T101500-1", "reason": word}),
        );
    }
}

// An engine plugin and a subscriber learn one shape: what the engine sends is
// what the subscriber reads, beside `type` and `session`.
#[test]
fn a_result_frame_is_what_the_engine_reported() {
    let from_engine = json!({"text": "hi there", "utterances": [{"text": "hi there", "definite": true}], "final": true});
    let recognition: Recognition = serde_json::from_value(from_engine.clone()).expect("decode");
    let mut frame = serde_json::to_value(Frame::Result {
        session: session(),
        recognition,
    })
    .expect("encode");
    let fields = frame.as_object_mut().expect("object");
    fields.remove("type");
    fields.remove("session");
    assert_eq!(frame, from_engine);
}

// Batch engines do not split sentences and may leave the field out.
#[test]
fn utterances_may_be_left_out() {
    let recognition: Recognition =
        serde_json::from_value(json!({"text": "hi", "final": true})).expect("decode");
    assert!(recognition.utterances.is_empty());
}
