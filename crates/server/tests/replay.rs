//! `anya replay`: a fixture served over the real transport, exactly as
//! written, to every connection.

mod common;

use std::time::{Duration, Instant};

use anya_server::{Address, ServerError, parse_address, start_replay};
use common::*;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

fn tcp() -> Address {
    parse_address("127.0.0.1:0").expect("address")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixtures_are_served_verbatim_to_every_connection() {
    // future.jsonl included: what a newer Anya sends arrives unchanged.
    for name in [
        "dictation.jsonl",
        "continuous.jsonl",
        "errors.jsonl",
        "future.jsonl",
    ] {
        let lines = fixture(name);
        let server = start_replay(lines.clone(), tcp(), false)
            .await
            .expect("start");
        for _ in 0..2 {
            let mut client = connect(&server).await;
            let (texts, close) = until_close(&mut client).await;
            assert_eq!(texts, lines, "{name}");
            let close = close.expect("a close frame");
            assert_eq!(close.code, CloseCode::Normal);
            assert_eq!(close.reason.as_str(), "end of replay");
        }
        server.stop().await;
    }
}

#[tokio::test]
async fn a_line_that_is_not_a_json_object_is_refused_by_number() {
    let dir = TempDir::new("badreplay");
    let socket = dir.path().join("r.sock");
    let hello = r#"{"type":"hello","name":"anya","version":"0.1.0","protocol":0}"#;
    for (lines, bad) in [
        (vec![hello, "[1, 2]"], 2),
        (vec!["not json"], 1),
        (vec![hello, hello, ""], 3),
        (vec![hello, r#""hello""#], 2),
    ] {
        let lines = lines.into_iter().map(str::to_owned).collect();
        let err = start_replay(lines, Address::Unix(socket.clone()), false)
            .await
            .expect_err("refused");
        assert!(
            matches!(&err, ServerError::Replay { line, .. } if *line == bad),
            "{err}"
        );
        assert!(err.to_string().starts_with(&format!("replay line {bad}: ")));
        assert!(
            std::fs::symlink_metadata(&socket).is_err(),
            "nothing listened"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_is_served_on_a_unix_socket() {
    let dir = TempDir::new("replay");
    let lines = fixture("continuous.jsonl");
    let server = start_replay(
        lines.clone(),
        Address::Unix(dir.path().join("r.sock")),
        false,
    )
    .await
    .expect("start");
    let mut client = connect(&server).await;
    assert_eq!(until_close(&mut client).await.0, lines);
    server.stop().await;
}

// dictation.jsonl: three preamble frames at once, then session_start (200
// ms), three results (500 ms each), session_end and source (200 ms each).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_realtime_replay_is_paced_like_a_session() {
    let lines = fixture("dictation.jsonl");
    let server = start_replay(lines.clone(), tcp(), true)
        .await
        .expect("start");
    let mut client = connect(&server).await;
    let started = Instant::now();
    let preamble = [
        next_text(&mut client).await,
        next_text(&mut client).await,
        next_text(&mut client).await,
    ];
    assert!(started.elapsed() < Duration::from_millis(150));
    assert_eq!(preamble.as_slice(), &lines[..3]);
    let (rest, _) = until_close(&mut client).await;
    assert_eq!(rest.as_slice(), &lines[3..]);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(2050) && elapsed < Duration::from_secs(4),
        "{elapsed:?}"
    );
    server.stop().await;
}
