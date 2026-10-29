//! `anya subscribe`: NDJSON on stdout, the ready and exit lines on stderr,
//! and why it ended.

mod common;

use std::time::Duration;

use anya_protocol::{Source, SourceState};
use anya_server::{
    Address, ExitReason, Server, SubscribeError, SubscribeExit, parse_address, start_replay,
    subscribe,
};
use common::*;

fn tcp() -> Address {
    parse_address("127.0.0.1:0").expect("address")
}

struct Run {
    exit: Result<SubscribeExit, SubscribeError>,
    stdout: String,
    stderr: Vec<String>,
}

async fn capture(address: Address, max_events: Option<u64>, timeout: Option<Duration>) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = tokio::time::timeout(
        WAIT,
        subscribe(address, max_events, timeout, &mut out, &mut err),
    )
    .await
    .expect("subscribe returned");
    Run {
        exit,
        stdout: String::from_utf8(out).expect("UTF-8"),
        stderr: String::from_utf8(err)
            .expect("UTF-8")
            .lines()
            .map(str::to_owned)
            .collect(),
    }
}

fn ndjson(lines: &[String]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Checks the two stderr lines and returns the seconds the exit line gives.
fn check_stderr(stderr: &[String], url: &str, frames: u64, reason: &str) -> f64 {
    assert_eq!(stderr.len(), 2, "{stderr:?}");
    assert_eq!(stderr[0], format!("[subscribe] ready addr={url}"));
    let prefix = format!("[subscribe] exited — received {frames} frame(s) in ");
    let suffix = format!("s (reason: {reason})");
    let seconds = stderr[1]
        .strip_prefix(&prefix)
        .and_then(|r| r.strip_suffix(&suffix))
        .unwrap_or_else(|| panic!("exit line: {}", stderr[1]));
    assert_eq!(seconds.split_once('.').map(|(_, ms)| ms.len()), Some(3));
    seconds.parse().expect("seconds")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_stops_at_the_limit() {
    let lines = fixture("continuous.jsonl");
    let server = start_replay(lines.clone(), tcp(), false)
        .await
        .expect("start");
    let run = capture(address_of(&server), Some(2), None).await;
    let exit = run.exit.expect("connected");
    assert_eq!(exit.reason, ExitReason::Limit);
    assert_eq!(exit.frames, 2);
    assert!(exit.connected);
    assert_eq!(run.stdout, ndjson(&lines[..2]));
    check_stderr(&run.stderr, &server.url(), 2, "limit");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_prints_everything_until_the_server_closes() {
    let lines = fixture("dictation.jsonl");
    let server = start_replay(lines.clone(), tcp(), false)
        .await
        .expect("start");
    // Zero is no bound, as on the command line.
    for (max_events, timeout) in [(None, None), (Some(0), Some(Duration::ZERO))] {
        let run = capture(address_of(&server), max_events, timeout).await;
        let exit = run.exit.expect("connected");
        assert_eq!(exit.reason, ExitReason::Closed);
        assert_eq!(exit.frames, lines.len() as u64);
        assert_eq!(run.stdout, ndjson(&lines));
        check_stderr(&run.stderr, &server.url(), lines.len() as u64, "closed");
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_stops_at_the_timeout() {
    let hub = hub();
    hub.set_source(Source::Continuous, SourceState::Listening, None);
    let server = Server::start(hub, tcp(), None).await.expect("start");
    let run = capture(address_of(&server), None, Some(Duration::from_millis(300))).await;
    let exit = run.exit.expect("connected");
    assert_eq!(exit.reason, ExitReason::Timeout);
    assert_eq!(exit.frames, 2);
    assert_eq!(run.stdout.lines().count(), 2);
    assert!(run.stdout.starts_with("{\"type\":\"hello\""));
    let seconds = check_stderr(&run.stderr, &server.url(), 2, "timeout");
    assert!(seconds < 0.3 + 0.2, "{seconds}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_reads_a_unix_socket() {
    let dir = TempDir::new("sub");
    let path = dir.path().join("s.sock");
    let lines = fixture("errors.jsonl");
    let server = start_replay(lines.clone(), Address::Unix(path.clone()), false)
        .await
        .expect("start");
    let run = capture(Address::Unix(path.clone()), Some(1), None).await;
    assert_eq!(run.exit.expect("connected").reason, ExitReason::Limit);
    assert_eq!(run.stdout, ndjson(&lines[..1]));
    check_stderr(&run.stderr, &format!("unix:{}", path.display()), 1, "limit");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failing_to_connect_is_the_only_error() {
    let dir = TempDir::new("nosock");
    let missing = Address::Unix(dir.path().join("missing.sock"));
    let run = capture(missing, None, None).await;
    assert!(matches!(run.exit, Err(SubscribeError::Connect { .. })));
    assert!(run.stdout.is_empty() && run.stderr.is_empty());

    let refused = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        Address::Tcp(listener.local_addr().expect("addr"))
    };
    let run = capture(refused, None, None).await;
    let err = run.exit.expect_err("refused");
    assert!(err.to_string().contains("is Anya running"), "{err}");
    assert!(run.stdout.is_empty() && run.stderr.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_timeout_covers_connecting() {
    // Accepts, and never answers the handshake.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = Address::Tcp(listener.local_addr().expect("addr"));
    let held = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        tokio::time::sleep(WAIT).await;
        drop(stream);
    });
    let run = capture(address, None, Some(Duration::from_millis(200))).await;
    let exit = run.exit.expect("a timeout is not an error");
    assert_eq!(exit.reason, ExitReason::Timeout);
    assert!(!exit.connected);
    assert!(run.stdout.is_empty() && run.stderr.is_empty());
    held.abort();
}
