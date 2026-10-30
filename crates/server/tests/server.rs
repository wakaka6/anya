//! The live server over real sockets: what a subscriber receives, what it
//! never receives, and how it is let go.

mod common;

use std::time::{Duration, Instant};

use anya_protocol::{Frame, Source, SourceState, contract};
use anya_server::{Address, Server, parse_address};
use common::*;
use tokio::net::TcpSocket;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};

fn tcp() -> Address {
    parse_address("127.0.0.1:0").expect("address")
}

fn device() -> Option<String> {
    Some("BlackHole 2ch".to_owned())
}

async fn snapshot_then_live(address: Address) {
    let hub = hub();
    let engine = FakeEngine::new();
    hub.set_source(Source::Continuous, SourceState::Listening, device());
    let session = open(&hub, &engine, Source::Continuous).await;
    session.push(rec(&[("Hello", false)], false)).await;
    let server = Server::start(hub.clone(), address, None, None)
        .await
        .expect("start");
    let mut client = connect(&server).await;

    let mut got = frames(&mut client, 4).await;
    assert!(
        matches!(&got[0], Frame::Hello { name, version, .. } if name == "anya" && version == "0.1.0-test")
    );
    assert_eq!(
        got[1],
        Frame::Source {
            source: Source::Continuous,
            state: SourceState::Listening,
            device: device(),
        }
    );
    assert!(
        matches!(&got[2], Frame::SessionStart { session: s, source: Source::Continuous, engine, .. } if s == session.live.id() && engine == "fake")
    );
    assert!(matches!(&got[3], Frame::Result { recognition, .. } if recognition.text == "Hello"));
    assert_eq!(server.subscribers(), 1);

    session.level().await;
    session
        .push(rec(&[("Hello.", true), (" World", false)], false))
        .await;
    session
        .push(rec(&[("Hello.", true), (" World.", true)], true))
        .await;
    hub.set_source(Source::Continuous, SourceState::Idle, device());
    got.extend(frames(&mut client, 5).await);
    assert!(matches!(got[4], Frame::Level { .. }));
    assert!(matches!(&got[6], Frame::Result { recognition, .. } if recognition.is_final));
    assert!(matches!(got[7], Frame::SessionEnd { .. }));
    assert_eq!(
        got[8],
        Frame::Source {
            source: Source::Continuous,
            state: SourceState::Idle,
            device: device(),
        }
    );
    if let Err(breach) = contract::check(&got) {
        panic!("{breach}: {got:?}");
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_subscriber_gets_the_snapshot_then_live_frames() {
    let hub = hub();
    let server = Server::start(hub, tcp(), None, None).await.expect("start");
    let url = server.url();
    let port = url
        .strip_prefix("ws://127.0.0.1:")
        .and_then(|r| r.strip_suffix("/asr"))
        .and_then(|p| p.parse::<u16>().ok())
        .expect("ws://127.0.0.1:<port>/asr");
    assert_ne!(port, 0, "the url names the bound port");
    server.stop().await;

    snapshot_then_live(tcp()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_subscriber_gets_the_snapshot_then_live_frames() {
    let dir = TempDir::new("live");
    snapshot_then_live(Address::Unix(dir.path().join("a.sock"))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dictation_never_reaches_a_subscriber() {
    let hub = hub();
    let engine = FakeEngine::new();
    hub.set_source(
        Source::Dictation,
        SourceState::Listening,
        Some("Mic".into()),
    );
    hub.set_source(Source::Continuous, SourceState::Listening, device());
    let dictation = open(&hub, &engine, Source::Dictation).await;
    dictation.push(rec(&[("secret", false)], false)).await;
    let continuous = open(&hub, &engine, Source::Continuous).await;
    let server = Server::start(hub.clone(), tcp(), None, None)
        .await
        .expect("start");
    let mut client = connect(&server).await;

    let mut got = frames(&mut client, 3).await;
    assert!(matches!(got[0], Frame::Hello { .. }));
    assert!(matches!(
        got[1],
        Frame::Source {
            source: Source::Continuous,
            ..
        }
    ));
    assert!(
        matches!(&got[2], Frame::SessionStart { session, .. } if session == continuous.live.id())
    );

    dictation.level().await;
    dictation
        .push(rec(&[("secret.", true), (" more", false)], false))
        .await;
    continuous.level().await;
    continuous.push(rec(&[("Public.", true)], true)).await;
    dictation
        .push(rec(&[("secret.", true), (" more.", true)], true))
        .await;
    hub.set_source(Source::Dictation, SourceState::Idle, None);
    let later = open(&hub, &engine, Source::Dictation).await;
    later.level().await;
    later.push(rec(&[("also secret", false)], true)).await;
    // The marker: everything published before it has been forwarded or not.
    hub.set_source(Source::Continuous, SourceState::Idle, device());
    loop {
        let frame = next_frame(&mut client).await;
        let marker = matches!(
            frame,
            Frame::Source {
                state: SourceState::Idle,
                ..
            }
        );
        got.push(frame);
        if marker {
            break;
        }
    }
    let dictated = [dictation.live.id(), later.live.id()];
    for frame in &got {
        let leaked = match frame {
            Frame::Source { source, .. } | Frame::SessionStart { source, .. } => {
                *source == Source::Dictation
            }
            Frame::Result { session, .. }
            | Frame::Level { session, .. }
            | Frame::SessionEnd { session, .. } => dictated.contains(&session),
            _ => false,
        };
        assert!(!leaked, "dictation reached a subscriber: {frame:?}");
    }
    assert_eq!(got.len(), 7, "{got:?}");
    assert_eq!(contract::check(&got), Ok(()));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_subscriber_is_dropped_without_stalling_others() {
    let hub = hub();
    let engine = FakeEngine::new();
    hub.set_source(Source::Continuous, SourceState::Listening, device());
    let session = open(&hub, &engine, Source::Continuous).await;
    let server = Server::start(hub.clone(), tcp(), None, None)
        .await
        .expect("start");
    let mut fast = connect(&server).await;
    frames(&mut fast, 3).await;

    // A subscriber that reads its snapshot and then nothing, with a small
    // receive buffer so the backlog shows soon.
    let addr = match address_of(&server) {
        Address::Tcp(addr) => addr,
        Address::Unix(_) => unreachable!("a tcp server"),
    };
    let socket = TcpSocket::new_v4().expect("socket");
    socket.set_recv_buffer_size(4096).expect("SO_RCVBUF");
    let stream = socket.connect(addr).await.expect("connect");
    let (mut slow, _) = tokio_tungstenite::client_async(
        format!("ws://{addr}/asr"),
        Box::new(stream) as Box<dyn Io>,
    )
    .await
    .expect("handshake");
    frames(&mut slow, 3).await;
    assert_eq!(server.subscribers(), 2);

    let big = "x".repeat(32 * 1024);
    let started = Instant::now();
    let mut pushed = 0;
    while server.subscribers() == 2 {
        assert!(pushed < 5000, "the slow subscriber was never dropped");
        session.push(rec(&[(big.as_str(), false)], false)).await;
        pushed += 1;
        // The other subscriber keeps up, frame by frame.
        assert!(matches!(next_frame(&mut fast).await, Frame::Result { .. }));
    }
    assert!(
        pushed > anya_core::hub::BUFFER,
        "dropped after only {pushed} frames"
    );
    assert!(started.elapsed() < Duration::from_secs(30));

    let (_, close) = until_close(&mut slow).await;
    let close = close.expect("a close frame");
    assert_eq!(close.code, CloseCode::Policy);
    assert!(close.reason.contains("256 frames behind"), "{close}");

    session.push(rec(&[("still here", false)], false)).await;
    assert!(
        matches!(next_frame(&mut fast).await, Frame::Result { recognition, .. } if recognition.text == "still here")
    );
    assert_eq!(server.subscribers(), 1);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribers_are_invisible_to_each_other() {
    let hub = hub();
    hub.set_source(Source::Continuous, SourceState::Listening, device());
    let server = Server::start(hub.clone(), tcp(), None, None)
        .await
        .expect("start");
    let mut a = connect(&server).await;
    let first = frames(&mut a, 2).await;
    let mut b = connect(&server).await;
    assert_eq!(frames(&mut b, 2).await, first);
    assert_eq!(server.subscribers(), 2);
    b.close(None).await.expect("close");
    drop(b);
    eventually(|| server.subscribers() == 1).await;

    // Neither the join nor the leave reached A: its next frame is this one.
    hub.set_source(Source::Continuous, SourceState::Idle, device());
    assert_eq!(
        next_frame(&mut a).await,
        Frame::Source {
            source: Source::Continuous,
            state: SourceState::Idle,
            device: device(),
        }
    );
    server.stop().await;
}

fn status(result: Result<Client, tungstenite::Error>) -> Option<u16> {
    match result {
        Ok(_) => None,
        Err(tungstenite::Error::Http(response)) => Some(response.status().as_u16()),
        Err(e) => panic!("expected an HTTP refusal, got {e}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_pages_off_loopback_and_other_paths_are_refused() {
    let server = Server::start(hub(), tcp(), None, None)
        .await
        .expect("start");
    let url = server.url();
    for origin in [
        "https://example.com",
        "http://192.168.1.20:7590",
        "null",
        "http://localhost.example.com",
    ] {
        assert_eq!(
            status(connect_with(&url, "/asr", Some(origin)).await),
            Some(403),
            "{origin}"
        );
    }
    for origin in [
        None,
        Some("http://localhost:5173"),
        Some("http://127.0.0.1:8080"),
        Some("http://[::1]"),
        Some("tauri://localhost"),
    ] {
        let mut client = connect_with(&url, "/asr", origin)
            .await
            .unwrap_or_else(|e| panic!("{origin:?} refused: {e}"));
        assert!(matches!(next_frame(&mut client).await, Frame::Hello { .. }));
    }
    assert_eq!(status(connect_with(&url, "/", None).await), Some(404));
    assert_eq!(status(connect_with(&url, "/asr/x", None).await), Some(404));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_a_subscriber_sends_is_ignored() {
    let hub = hub();
    let server = Server::start(hub.clone(), tcp(), None, None)
        .await
        .expect("start");
    let mut client = connect(&server).await;
    assert!(matches!(next_frame(&mut client).await, Frame::Hello { .. }));
    client
        .send(Message::text("{\"type\":\"subscribe\"}"))
        .await
        .expect("text");
    client
        .send(Message::binary(vec![1, 2, 3]))
        .await
        .expect("binary");
    client
        .send(Message::Ping(vec![9].into()))
        .await
        .expect("ping");
    hub.set_source(Source::Continuous, SourceState::Listening, None);
    assert!(matches!(
        next_frame(&mut client).await,
        Frame::Source {
            state: SourceState::Listening,
            ..
        }
    ));
    assert_eq!(server.subscribers(), 1);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_disconnects_everyone_and_refuses_new_subscribers() {
    let server = Server::start(hub(), tcp(), None, None)
        .await
        .expect("start");
    let url = server.url();
    let mut client = connect(&server).await;
    assert!(matches!(next_frame(&mut client).await, Frame::Hello { .. }));
    let started = Instant::now();
    let ((), (_, close)) = tokio::join!(server.stop(), until_close(&mut client));
    let close = close.expect("a close frame");
    assert_eq!(close.code, CloseCode::Away);
    assert!(started.elapsed() < Duration::from_secs(2), "stop waited");
    assert!(connect_with(&url, "/asr", None).await.is_err());
}
