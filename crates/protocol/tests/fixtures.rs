//! The streams in `fixtures/` are what downstream developers test their
//! parsers against, so they must be exactly what Anya sends and keep every
//! promise in docs/protocol.md.

use std::path::Path;

use anya_protocol::{EndReason, Frame, Source, SourceState, contract};
use serde_json::Value;

fn lines(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("read fixture");
    text.lines()
        .map(|l| serde_json::from_str(l).expect("a JSON object per line"))
        .collect()
}

fn frames(lines: &[Value]) -> Vec<Frame> {
    lines
        .iter()
        .map(|l| serde_json::from_value(l.clone()).expect("decode"))
        .collect()
}

#[test]
fn current_fixtures_are_what_anya_sends_and_keep_the_promises() {
    for name in ["dictation.jsonl", "continuous.jsonl", "errors.jsonl"] {
        let lines = lines(name);
        let frames = frames(&lines);
        for (line, frame) in lines.iter().zip(&frames) {
            assert_eq!(
                &serde_json::to_value(frame).expect("encode"),
                line,
                "{name}"
            );
        }
        if let Err(breach) = contract::check(&frames) {
            panic!("{name}: {breach}");
        }
    }
}

// What a newer Anya may send within the same protocol version: a new frame
// type, source, state and reason, and new fields. All of it reads; nothing fails.
#[test]
fn a_newer_anya_is_read_not_refused() {
    let frames = frames(&lines("future.jsonl"));
    assert_eq!(contract::check(&frames), Ok(()));
    assert!(matches!(
        frames[1],
        Frame::Source {
            source: Source::Unknown,
            ..
        }
    ));
    assert!(matches!(
        frames[2],
        Frame::Source {
            state: SourceState::Unknown,
            ..
        }
    ));
    assert_eq!(frames[3], Frame::Unknown);
    let Frame::Result { recognition, .. } = &frames[5] else {
        panic!("frame 5 is a result: {:?}", frames[5]);
    };
    assert_eq!(recognition.text, "Hello.");
    assert!(recognition.utterances[0].definite);
    assert!(matches!(
        frames[6],
        Frame::SessionEnd {
            reason: EndReason::Unknown,
            ..
        }
    ));
}
