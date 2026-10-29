//! The engine against a local server that speaks the service's protocol.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{EngineError, Output, Session, SessionParams};
use anya_core::hub::{Hub, HubInfo};
use anya_protocol::{EndReason, ErrorCode, FieldKind, Frame, Source, contract};
use flate2::Compression;
use flate2::write::GzEncoder;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue};

const WAIT: Duration = Duration::from_secs(5);

type Server = WebSocketStream<TcpStream>;

async fn listen() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("address");
    (listener, format!("ws://{addr}"))
}

/// Accepts one connection, answering with `logid`; returns the request's
/// path and headers.
#[expect(
    clippy::result_large_err,
    reason = "tungstenite's handshake callback returns its own ErrorResponse"
)]
async fn accept(listener: &TcpListener, logid: &'static str) -> (Server, String, HeaderMap) {
    let (tcp, _) = listener.accept().await.expect("accept");
    let mut seen = (String::new(), HeaderMap::new());
    let ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, mut resp: Response| {
        seen = (req.uri().path().to_owned(), req.headers().clone());
        resp.headers_mut()
            .insert("x-tt-logid", HeaderValue::from_static(logid));
        Ok::<Response, ErrorResponse>(resp)
    })
    .await
    .expect("handshake");
    (ws, seen.0, seen.1)
}

/// The next client message, checked against the header it must carry;
/// returns its payload.
async fn expect(ws: &mut Server, header: [u8; 4]) -> Vec<u8> {
    let msg = tokio::time::timeout(WAIT, ws.next())
        .await
        .expect("a client message in time")
        .expect("open")
        .expect("readable");
    let Message::Binary(bytes) = msg else {
        panic!("expected a binary message, got {msg:?}");
    };
    assert_eq!(bytes[..4], header, "header of {bytes:02x?}");
    let size = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    assert_eq!(bytes.len(), 8 + size, "size field matches the payload");
    bytes[8..].to_vec()
}

/// A full server response as the service sends it: always a sequence,
/// flags `0b0001`, or `0b0011` on the last packet.
fn response(sequence: i32, last: bool, gzip: bool, result: Value) -> Message {
    let mut payload = json!({ "audio_info": { "duration": 1000 }, "result": result })
        .to_string()
        .into_bytes();
    if gzip {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&payload).expect("gzip");
        payload = enc.finish().expect("gzip");
    }
    let flags = if last { 0b0011 } else { 0b0001 };
    let mut out = vec![0x11, 0x90 | flags, 0x10 | u8::from(gzip), 0x00];
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Message::Binary(out.into())
}

fn error_frame(code: u32, message: &str) -> Message {
    let mut out = vec![0x11, 0xf0, 0x10, 0x00];
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message.as_bytes());
    Message::Binary(out.into())
}

async fn expect_close(ws: &mut Server) {
    let next = tokio::time::timeout(WAIT, ws.next())
        .await
        .expect("the client closes in time");
    assert!(
        matches!(next, None | Some(Ok(Message::Close(_))) | Some(Err(_))),
        "expected the socket to close, got {next:?}"
    );
}

fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn hub() -> Hub {
    Hub::new(HubInfo {
        version: "test".to_owned(),
        session_prefix: "t".to_owned(),
    })
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Frames of one subscription up to the first `session_end`.
async fn frames_until_end(sub: anya_core::hub::Subscription) -> Vec<Frame> {
    let mut frames = sub.snapshot;
    let mut live = sub.frames;
    loop {
        let frame = tokio::time::timeout(WAIT, live.recv())
            .await
            .expect("frames in time")
            .expect("not lagging");
        let end = matches!(frame, Frame::SessionEnd { .. });
        frames.push(frame);
        if end {
            return frames;
        }
    }
}

/// A result as text, sentences with their definite flag, and final.
type Seen = (String, Vec<(String, bool)>, bool);

fn results(frames: &[Frame]) -> Vec<Seen> {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::Result { recognition: r, .. } => Some((
                r.text.clone(),
                r.utterances
                    .iter()
                    .map(|u| (u.text.clone(), u.definite))
                    .collect(),
                r.is_final,
            )),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_dictated_session_streams_packets_and_settles_sentences() {
    let (listener, origin) = listen().await;
    let server = tokio::spawn(async move {
        let (mut ws, path, headers) = accept(&listener, "log-1").await;
        let init = expect(&mut ws, [0x11, 0x10, 0x10, 0x00]).await;
        let init: Value = serde_json::from_slice(&init).expect("init is JSON");
        ws.send(response(1, false, false, json!({ "text": "" })))
            .await
            .expect("ack");

        assert_eq!(expect(&mut ws, [0x11, 0x20, 0x00, 0x00]).await.len(), 6400);
        ws.send(response(
            2,
            false,
            false,
            json!({ "text": "今天", "utterances": [{ "text": "今天", "definite": false }] }),
        ))
        .await
        .expect("partial");

        assert_eq!(expect(&mut ws, [0x11, 0x20, 0x00, 0x00]).await.len(), 6400);
        ws.send(response(
            3,
            false,
            true,
            json!({ "text": "今天天气怎么样？我们", "utterances": [
                { "text": "今天天气怎么样？", "definite": true },
                { "text": "我们", "definite": false },
            ]}),
        ))
        .await
        .expect("definite");

        assert_eq!(expect(&mut ws, [0x11, 0x22, 0x00, 0x00]).await.len(), 1280);
        ws.send(response(
            -4,
            true,
            false,
            json!({ "text": "今天天气怎么样？我们走吧。", "utterances": [
                { "text": "今天天气怎么样？", "definite": true },
                { "text": "我们走吧。", "definite": true },
            ]}),
        ))
        .await
        .expect("final");
        expect_close(&mut ws).await;
        (path, headers, init)
    });

    let hub = hub();
    let sub = hub.subscribe();
    let engine = anya_engine_doubao::engine();
    let params = SessionParams {
        language: Some("zh-CN".to_owned()),
        hotwords: vec!["Anya".to_owned(), " Anya ".to_owned(), "Wayland".to_owned()],
        settings: settings(&[("api_key", "test-key"), ("base_host", &origin)]),
    };
    let session = hub.open(engine.as_ref(), Source::Dictation, params);
    for _ in 0..11 {
        assert!(session.send(Chunk(vec![1; 1280])).await);
    }
    let outcome = session.finish(WAIT).await;
    assert_eq!(outcome.reason, EndReason::Final);
    assert_eq!(outcome.text, "今天天气怎么样？我们走吧。");

    let frames = frames_until_end(sub).await;
    assert_eq!(contract::check(&frames), Ok(()));
    let s = |t: &str| t.to_owned();
    assert_eq!(
        results(&frames),
        vec![
            (s("今天"), vec![(s("今天"), false)], false),
            (
                s("今天天气怎么样？我们"),
                vec![(s("今天天气怎么样？"), true), (s("我们"), false)],
                false
            ),
            (
                s("今天天气怎么样？我们走吧。"),
                vec![(s("今天天气怎么样？"), true), (s("我们走吧。"), true)],
                true
            ),
        ]
    );

    let (path, headers, init) = server.await.expect("server");
    assert_eq!(path, "/api/v3/sauc/bigmodel_async");
    assert_eq!(header(&headers, "x-api-key"), Some("test-key"));
    assert_eq!(
        header(&headers, "x-api-resource-id"),
        Some("volc.seedasr.sauc.duration")
    );
    assert_eq!(header(&headers, "x-api-sequence"), Some("-1"));
    assert_eq!(header(&headers, "x-api-request-id").map(str::len), Some(36));
    assert_eq!(header(&headers, "x-api-connect-id").map(str::len), Some(36));
    assert_eq!(header(&headers, "x-api-access-key"), None);
    assert_eq!(
        init["audio"],
        json!({ "format": "pcm", "rate": 16000, "bits": 16, "channel": 1,
                "codec": "raw", "language": "zh-CN" })
    );
    assert_eq!(init["request"]["model_name"], "bigmodel");
    assert_eq!(init["request"]["enable_nonstream"], true);
    assert_eq!(init["request"]["show_utterances"], true);
    assert_eq!(
        init["request"]["corpus"]["context"],
        r#"{"hotwords":[{"word":"Anya"},{"word":"Wayland"}]}"#
    );
}

#[tokio::test]
async fn a_server_error_frame_ends_the_session_with_its_words() {
    let (listener, origin) = listen().await;
    let server = tokio::spawn(async move {
        let (mut ws, ..) = accept(&listener, "log-2").await;
        expect(&mut ws, [0x11, 0x10, 0x10, 0x00]).await;
        expect(&mut ws, [0x11, 0x20, 0x00, 0x00]).await;
        ws.send(error_frame(45_000_151, "invalid audio format"))
            .await
            .expect("error frame");
        expect_close(&mut ws).await;
    });

    let hub = hub();
    let sub = hub.subscribe();
    let engine = anya_engine_doubao::engine();
    let params = SessionParams {
        settings: settings(&[("api_key", "k"), ("base_host", &origin)]),
        ..SessionParams::default()
    };
    let mut session = hub.open(engine.as_ref(), Source::Continuous, params);
    for _ in 0..5 {
        session.send(Chunk(vec![0; 1280])).await;
    }
    let outcome = tokio::time::timeout(WAIT, session.ended())
        .await
        .expect("ends in time");
    assert_eq!(outcome.reason, EndReason::Error);
    assert_eq!(outcome.error, Some(ErrorCode::Upstream));
    assert_eq!(
        outcome.detail.as_deref(),
        Some("45000151: invalid audio format (logid log-2)")
    );
    let frames = frames_until_end(sub).await;
    assert_eq!(contract::check(&frames), Ok(()));
    server.await.expect("server");
}

#[tokio::test]
async fn dropping_the_input_closes_the_socket() {
    let (listener, origin) = listen().await;
    let server = tokio::spawn(async move {
        let (mut ws, ..) = accept(&listener, "log-3").await;
        expect(&mut ws, [0x11, 0x10, 0x10, 0x00]).await;
        expect_close(&mut ws).await;
    });
    let engine = anya_engine_doubao::engine();
    let Session { input, mut output } = engine.open(SessionParams {
        settings: settings(&[("api_key", "k"), ("base_host", &origin)]),
        ..SessionParams::default()
    });
    let ready = tokio::time::timeout(WAIT, output.recv())
        .await
        .expect("ready");
    assert!(matches!(ready, Some(Output::Ready)), "{ready:?}");
    drop(input);
    tokio::time::timeout(WAIT, server)
        .await
        .expect("the server sees the close in time")
        .expect("server");
    let after = tokio::time::timeout(WAIT, output.recv())
        .await
        .expect("ends");
    assert!(after.is_none(), "no result after abandoning: {after:?}");
}

#[tokio::test]
async fn a_rejected_handshake_is_a_connect_error_with_status_and_body() {
    let (listener, origin) = listen().await;
    let server = tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = tcp.read(&mut buf).await.expect("read");
            assert!(n > 0, "request ended early");
            request.extend_from_slice(&buf[..n]);
        }
        let body = r#"{"error":"bad key"}"#;
        let reply = format!(
            "HTTP/1.1 403 Forbidden\r\nX-Tt-Logid: log-4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tcp.write_all(reply.as_bytes()).await.expect("write");
    });
    let engine = anya_engine_doubao::engine();
    let Session {
        input: _input,
        mut output,
    } = engine.open(SessionParams {
        settings: settings(&[("api_key", "wrong"), ("base_host", &origin)]),
        ..SessionParams::default()
    });
    let first = tokio::time::timeout(WAIT, output.recv())
        .await
        .expect("answer");
    let Some(Output::Error(EngineError::Connect(detail))) = first else {
        panic!("expected a connect error, got {first:?}");
    };
    assert!(detail.starts_with("HTTP 403 Forbidden"), "{detail}");
    assert!(detail.contains("bad key"), "{detail}");
    assert!(detail.ends_with("(logid log-4)"), "{detail}");
    server.await.expect("server");
}

#[tokio::test]
async fn legacy_credentials_and_a_silent_session() {
    let (listener, origin) = listen().await;
    let server = tokio::spawn(async move {
        let (mut ws, _, headers) = accept(&listener, "log-5").await;
        expect(&mut ws, [0x11, 0x10, 0x10, 0x00]).await;
        // No audio was spoken: no audio packet, the client says goodbye.
        expect_close(&mut ws).await;
        headers
    });
    let engine = anya_engine_doubao::engine();
    let Session { input, mut output } = engine.open(SessionParams {
        settings: settings(&[
            ("app_id", "app"),
            ("access_key", "secret"),
            ("base_host", &origin),
        ]),
        ..SessionParams::default()
    });
    input
        .send(anya_core::engine::Input::Finish)
        .await
        .expect("finish");
    let ready = tokio::time::timeout(WAIT, output.recv())
        .await
        .expect("ready");
    assert!(matches!(ready, Some(Output::Ready)), "{ready:?}");
    let last = tokio::time::timeout(WAIT, output.recv())
        .await
        .expect("final");
    let Some(Output::Result(r)) = last else {
        panic!("expected the final result, got {last:?}");
    };
    assert!(r.is_final && r.text.is_empty() && r.utterances.is_empty());

    let headers = server.await.expect("server");
    assert_eq!(header(&headers, "x-api-app-id"), Some("app"));
    assert_eq!(header(&headers, "x-api-app-key"), Some("app"));
    assert_eq!(header(&headers, "x-api-access-key"), Some("secret"));
    assert_eq!(header(&headers, "x-api-key"), None);
    assert_eq!(
        header(&headers, "x-api-resource-id"),
        Some("volc.bigasr.sauc.duration")
    );
}

#[tokio::test]
async fn missing_credentials_fail_before_connecting() {
    let engine = anya_engine_doubao::engine();
    let Session {
        input: _input,
        mut output,
    } = engine.open(SessionParams::default());
    let first = tokio::time::timeout(WAIT, output.recv())
        .await
        .expect("answer");
    assert!(
        matches!(&first, Some(Output::Error(EngineError::NotConfigured(k))) if k == "api_key"),
        "{first:?}"
    );
}

/// Reaches the real service over TLS with a key it must refuse. Needs the
/// network: `cargo test -p anya-engine-doubao -- --ignored`.
#[tokio::test]
#[ignore = "needs the network"]
async fn the_real_service_refuses_an_unknown_key() {
    let engine = anya_engine_doubao::engine();
    let Session {
        input: _input,
        mut output,
    } = engine.open(SessionParams {
        settings: settings(&[("api_key", "anya-smoke-test")]),
        ..SessionParams::default()
    });
    let first = tokio::time::timeout(Duration::from_secs(20), output.recv())
        .await
        .expect("an answer in time");
    let Some(Output::Error(EngineError::Connect(detail))) = first else {
        panic!("expected a connect error, got {first:?}");
    };
    println!("{detail}");
    assert!(detail.starts_with("HTTP 4"), "{detail}");
}

#[test]
fn check_takes_an_api_key_or_an_app_id_with_an_access_key() {
    let engine = anya_engine_doubao::engine();
    let missing = |k: &str| Err(EngineError::NotConfigured(k.to_owned()));
    assert_eq!(engine.check(&settings(&[])), missing("api_key"));
    assert_eq!(engine.check(&settings(&[("api_key", "k")])), Ok(()));
    assert_eq!(
        engine.check(&settings(&[("app_id", "a"), ("access_key", "s")])),
        Ok(())
    );
    assert_eq!(
        engine.check(&settings(&[("app_id", "a")])),
        missing("access_key")
    );
    assert_eq!(
        engine.check(&settings(&[("access_key", "s")])),
        missing("app_id")
    );
    assert_eq!(
        engine.check(&settings(&[("api_key", "k"), ("model_version", "3.0")])),
        missing("model_version")
    );
}

#[test]
fn the_manifest_lists_the_settings_in_both_languages() {
    let engine = anya_engine_doubao::engine();
    let m = engine.manifest();
    assert_eq!(m.id, "doubao");
    assert!(m.streaming);
    assert_eq!(m.title.get("zh-CN"), "豆包语音");
    assert_eq!(m.title.get("en"), "Doubao");
    assert_eq!(
        m.blurb.get("zh-CN"),
        "豆包大模型双向流式 ASR，边说边出字 + 二遍校正，支持热词"
    );
    assert_eq!(
        m.docs.as_deref(),
        Some("https://www.volcengine.com/docs/6561/1395867")
    );
    let shape: Vec<(&str, &str, bool, bool, bool)> = m
        .fields
        .iter()
        .map(|f| {
            let kind = match &f.kind {
                FieldKind::Text => "text",
                FieldKind::Secret => "secret",
                FieldKind::Choice { .. } => "choice",
            };
            (
                f.key.as_str(),
                kind,
                f.required,
                f.advanced,
                f.help.is_empty(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("api_key", "secret", false, false, false),
            ("app_id", "text", false, true, false),
            ("access_key", "secret", true, true, false),
            ("model_version", "choice", false, false, false),
            ("stream_billing", "choice", false, true, false),
            ("resource_id", "text", false, true, false),
            ("enable_lid", "choice", false, true, false),
            ("base_host", "text", false, true, false),
        ]
    );
    let options = |key: &str| {
        m.fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| match &f.kind {
                FieldKind::Choice { options } => Some(options.clone()),
                _ => None,
            })
    };
    assert_eq!(
        options("model_version"),
        Some(vec!["1.0".into(), "2.0".into()])
    );
    assert_eq!(
        options("stream_billing"),
        Some(vec!["duration".into(), "concurrent".into()])
    );
    assert_eq!(
        options("enable_lid"),
        Some(vec!["false".into(), "true".into()])
    );
    for f in &m.fields {
        for locale in ["en", "zh-CN"] {
            assert!(
                !f.label.get(locale).is_empty(),
                "{} label in {locale}",
                f.key
            );
            assert!(!f.help.get(locale).is_empty(), "{} help in {locale}", f.key);
        }
        assert_ne!(
            f.help.get("en"),
            f.help.get("zh-CN"),
            "{} help is translated",
            f.key
        );
    }
    assert_eq!(m.fields[3].label.get("zh-CN"), "模型版本");
}
