//! The JSON a gateway client sends and the replies only it receives. Changing
//! an expectation here is a protocol change, like a frame's.

use std::path::Path;

use anya_protocol::{
    ClientMessage, Frame, PairingOffer, Refusal, Reply, Scope, SessionId, contract,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[track_caller]
fn pinned<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T, wire: Value) {
    assert_eq!(serde_json::to_value(&value).expect("encode"), wire);
    let read: T = serde_json::from_value(wire).expect("decode");
    assert_eq!(read, value);
}

fn session() -> SessionId {
    SessionId("anya-20261030T101500-9".to_owned())
}

#[test]
fn client_messages() {
    pinned(
        ClientMessage::Pair {
            code: "482913".to_owned(),
            name: "iPhone".to_owned(),
        },
        json!({"type": "pair", "code": "482913", "name": "iPhone"}),
    );
    pinned(
        ClientMessage::DictationStart {
            request_id: "ios-42".to_owned(),
            language: Some("zh-CN".to_owned()),
            hotwords: vec!["Anya".to_owned()],
        },
        json!({"type": "dictation_start", "request_id": "ios-42", "language": "zh-CN", "hotwords": ["Anya"]}),
    );
    pinned(
        ClientMessage::DictationStart {
            request_id: "r".to_owned(),
            language: None,
            hotwords: Vec::new(),
        },
        json!({"type": "dictation_start", "request_id": "r"}),
    );
    pinned(
        ClientMessage::DictationStop { session: session() },
        json!({"type": "dictation_stop", "session": "anya-20261030T101500-9"}),
    );
    pinned(
        ClientMessage::DictationCancel { session: session() },
        json!({"type": "dictation_cancel", "session": "anya-20261030T101500-9"}),
    );
}

// A newer client's message reads as Unknown, so Anya can refuse it by code
// instead of dropping the connection.
#[test]
fn a_newer_client_message_reads_as_unknown() {
    let read: ClientMessage =
        serde_json::from_value(json!({"type": "translate_start", "to": "en"})).expect("decode");
    assert_eq!(read, ClientMessage::Unknown);
}

#[test]
fn replies() {
    pinned(
        Reply::Paired {
            client: "c-1".to_owned(),
            token: "t0k3n".to_owned(),
            scopes: vec![Scope::Dictate],
        },
        json!({"type": "paired", "client": "c-1", "token": "t0k3n", "scopes": ["dictate"]}),
    );
    pinned(
        Reply::DictationAccepted {
            request_id: "ios-42".to_owned(),
            session: session(),
            streaming: false,
        },
        json!({"type": "dictation_accepted", "request_id": "ios-42", "session": "anya-20261030T101500-9", "streaming": false}),
    );
    pinned(
        Reply::Refused {
            request_id: Some("ios-43".to_owned()),
            session: None,
            reason: Refusal::Busy,
            detail: None,
        },
        json!({"type": "refused", "request_id": "ios-43", "reason": "busy"}),
    );
    pinned(
        Reply::Refused {
            request_id: None,
            session: Some(session()),
            reason: Refusal::UnknownSession,
            detail: Some("ended".to_owned()),
        },
        json!({"type": "refused", "session": "anya-20261030T101500-9", "reason": "unknown_session", "detail": "ended"}),
    );
}

#[test]
fn unknown_scopes_and_refusals_read_without_failing() {
    let scopes: Vec<Scope> =
        serde_json::from_value(json!(["dictate", "control_continuous"])).expect("decode");
    assert_eq!(scopes, [Scope::Dictate, Scope::Unknown]);
    let reply: Reply = serde_json::from_value(json!({"type": "refused", "reason": "rate_limited"}))
        .expect("decode");
    assert!(matches!(
        reply,
        Reply::Refused {
            reason: Refusal::Unknown,
            ..
        }
    ));
}

#[test]
fn a_pairing_offer_is_what_the_qr_code_holds() {
    pinned(
        PairingOffer {
            url: "wss://192.168.1.23:7591/asr".to_owned(),
            code: "482913".to_owned(),
            fingerprint: "3f:9a".to_owned(),
        },
        json!({"url": "wss://192.168.1.23:7591/asr", "code": "482913", "fingerprint": "3f:9a"}),
    );
}

// What a remote dictation client receives: replies and frames in one stream.
// The frames among them keep every promise; a subscriber-only reader skips
// the replies as frame types it does not know.
#[test]
fn the_gateway_fixture_is_replies_and_frames_that_keep_the_promises() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/gateway.jsonl");
    let text = std::fs::read_to_string(path).expect("read fixture");
    let mut frames = Vec::new();
    for line in text.lines() {
        let value: Value = serde_json::from_str(line).expect("a JSON object per line");
        match serde_json::from_value::<Reply>(value.clone()).expect("decode") {
            Reply::Unknown => {
                let frame: Frame = serde_json::from_value(value.clone()).expect("decode");
                assert_eq!(serde_json::to_value(&frame).expect("encode"), value);
                frames.push(frame);
            }
            reply => {
                assert_eq!(serde_json::to_value(&reply).expect("encode"), value);
                let as_frame: Frame = serde_json::from_value(value).expect("decode");
                assert_eq!(as_frame, Frame::Unknown, "a reply is no frame");
            }
        }
    }
    if let Err(breach) = contract::check(&frames) {
        panic!("gateway.jsonl: {breach}");
    }
}
