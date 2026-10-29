//! Sessions against a local stand-in for DashScope's WebSocket API.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{EngineError, Input, Output, SessionParams};
use anya_core::hub::{Hub, HubInfo};
use anya_protocol::{EndReason, ErrorCode, Frame, Source, contract};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

type Server = WebSocketStream<TcpStream>;

/// Listens on a free port; returns the `base_url` and the listener.
async fn listen() -> (String, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    (
        format!("ws://127.0.0.1:{port}/api-ws/v1/inference/"),
        listener,
    )
}

/// Accepts one WebSocket, recording its `Authorization` header.
// The callback's error type is tungstenite's handshake response.
#[allow(clippy::result_large_err)]
async fn accept(listener: &TcpListener, auth: Arc<Mutex<Option<String>>>) -> Server {
    let (tcp, _) = listener.accept().await.expect("accept");
    tokio_tungstenite::accept_hdr_async(tcp, move |req: &Request, res: Response| {
        let header = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        *auth.lock().expect("lock") = header;
        Ok(res)
    })
    .await
    .expect("handshake")
}

/// The next text frame, as JSON; binary frames are pushed onto `audio`.
async fn next_command(ws: &mut Server, audio: &mut Vec<u8>) -> Value {
    loop {
        match ws.next().await.expect("open").expect("frame") {
            Message::Text(t) => return serde_json::from_str(&t).expect("json"),
            Message::Binary(b) => audio.extend_from_slice(&b),
            _ => {}
        }
    }
}

async fn event(ws: &mut Server, value: Value) {
    ws.send(Message::text(value.to_string()))
        .await
        .expect("send");
}

fn header(event: &str) -> Value {
    json!({"task_id": "t", "event": event, "attributes": {}})
}

fn sentence(text: &str, end: bool) -> Value {
    json!({
        "header": header("result-generated"),
        "payload": {"output": {"sentence": {"begin_time": 0, "end_time": null, "text": text,
            "heartbeat": false, "sentence_end": end}}, "usage": null},
    })
}

fn settings(url: &str, extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut s = BTreeMap::from([
        ("api_key".to_owned(), "sk-test".to_owned()),
        ("base_url".to_owned(), url.to_owned()),
    ]);
    s.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
    );
    s
}

fn hub() -> Hub {
    Hub::new(HubInfo {
        version: "0.0.0".into(),
        session_prefix: "test".into(),
    })
}

fn chunk(n: u8) -> Chunk {
    Chunk(
        (0..640)
            .flat_map(|i| (i16::from(n) * 100 + i % 7).to_le_bytes())
            .collect(),
    )
}

/// What the server saw of a session.
struct Seen {
    run: Value,
    finish: Value,
    audio: Vec<u8>,
}

#[tokio::test]
async fn a_session_runs_from_run_task_to_task_finished() {
    let (url, listener) = listen().await;
    let auth = Arc::new(Mutex::new(None));
    let server = tokio::spawn({
        let auth = Arc::clone(&auth);
        async move {
            let mut ws = accept(&listener, auth).await;
            let mut audio = Vec::new();
            let run = next_command(&mut ws, &mut audio).await;
            event(
                &mut ws,
                json!({"header": header("task-started"), "payload": {}}),
            )
            .await;
            event(&mut ws, sentence("你好", false)).await;
            // What the client ignores: heartbeats, unknown events, non-JSON.
            event(&mut ws, json!({"header": header("result-generated"),
                "payload": {"output": {"sentence": {"text": "", "heartbeat": true, "sentence_end": false}}}})).await;
            event(&mut ws, json!({"header": header("something-new")})).await;
            ws.send(Message::text("not json")).await.expect("send");
            event(&mut ws, sentence("你好。", true)).await;
            event(&mut ws, sentence("今天天气", false)).await;
            let finish = next_command(&mut ws, &mut audio).await;
            event(&mut ws, sentence("今天天气不错。", true)).await;
            event(
                &mut ws,
                json!({"header": header("task-finished"),
                "payload": {"output": {}, "usage": null}}),
            )
            .await;
            Seen { run, finish, audio }
        }
    });

    let hub = hub();
    let sub = hub.subscribe();
    let mut frames = sub.snapshot;
    let mut live = sub.frames;
    let engine = anya_engine_dashscope::engine();
    let params = SessionParams {
        language: Some("zh-CN".into()),
        hotwords: vec!["天气".into()],
        settings: settings(&url, &[]),
    };
    let session = hub.open(engine.as_ref(), Source::Dictation, params);
    let mut sent = Vec::new();
    for n in 1..=3 {
        sent.extend_from_slice(&chunk(n).0);
        assert!(session.send(chunk(n)).await);
    }
    let outcome = session.finish(Duration::from_secs(5)).await;
    assert_eq!(outcome.reason, EndReason::Final);
    assert_eq!(outcome.text, "你好。今天天气不错。");

    let seen = server.await.expect("server");
    assert_eq!(
        auth.lock().expect("lock").as_deref(),
        Some("Bearer sk-test")
    );
    let task_id = seen.run["header"]["task_id"].as_str().expect("task id");
    assert_eq!(task_id.len(), 36);
    assert_eq!(
        seen.run,
        json!({
            "header": {"action": "run-task", "task_id": task_id, "streaming": "duplex"},
            "payload": {"task_group": "audio", "task": "asr", "function": "recognition",
                "model": "fun-asr-realtime",
                "parameters": {"format": "pcm", "sample_rate": 16000, "language_hints": ["zh"]},
                "input": {}},
        })
    );
    assert_eq!(
        seen.finish,
        json!({"header": {"action": "finish-task", "task_id": task_id, "streaming": "duplex"},
            "payload": {"input": {}}})
    );
    assert_eq!(seen.audio, sent);

    while let Ok(f) = live.try_recv() {
        frames.push(f);
    }
    contract::check(&frames).expect("frames keep the promises");
    let results: Vec<_> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Result { recognition, .. } => Some(recognition),
            _ => None,
        })
        .collect();
    let texts: Vec<_> = results.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "你好",
            "你好。",
            "你好。今天天气",
            "你好。今天天气不错。",
            "你好。今天天气不错。"
        ]
    );
    assert!(!results[2].utterances[1].definite);
    let last = results.last().expect("final");
    assert!(last.is_final && last.utterances.iter().all(|u| u.definite));
    assert!(matches!(
        frames.last(),
        Some(Frame::SessionEnd {
            reason: EndReason::Final,
            ..
        })
    ));
}

#[tokio::test]
async fn eight_k_models_get_halved_audio() {
    let (url, listener) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(&listener, Arc::default()).await;
        let mut audio = Vec::new();
        let run = next_command(&mut ws, &mut audio).await;
        event(
            &mut ws,
            json!({"header": header("task-started"), "payload": {}}),
        )
        .await;
        let finish = next_command(&mut ws, &mut audio).await;
        event(
            &mut ws,
            json!({"header": header("task-finished"), "payload": {}}),
        )
        .await;
        Seen { run, finish, audio }
    });
    let hub = hub();
    let params = SessionParams {
        settings: settings(&url, &[("model", "fun-asr-flash-8k-realtime")]),
        ..SessionParams::default()
    };
    let session = hub.open(
        anya_engine_dashscope::engine().as_ref(),
        Source::Continuous,
        params,
    );
    let constant = Chunk([0x10, 0x02].repeat(640));
    for _ in 0..3 {
        assert!(session.send(constant.clone()).await);
    }
    let outcome = session.finish(Duration::from_secs(5)).await;
    assert_eq!(outcome.reason, EndReason::Final);
    assert_eq!(outcome.text, "");
    let seen = server.await.expect("server");
    assert_eq!(seen.run["payload"]["model"], "fun-asr-flash-8k-realtime");
    assert_eq!(
        seen.run["payload"]["parameters"],
        json!({"format": "pcm", "sample_rate": 8000})
    );
    assert_eq!(seen.finish["header"]["action"], "finish-task");
    assert_eq!(seen.audio, [0x10, 0x02].repeat(3 * 320));
}

#[tokio::test]
async fn task_failed_ends_the_session_with_the_services_words() {
    let (url, listener) = listen().await;
    tokio::spawn(async move {
        let mut ws = accept(&listener, Arc::default()).await;
        let mut audio = Vec::new();
        next_command(&mut ws, &mut audio).await;
        event(
            &mut ws,
            json!({"header": header("task-started"), "payload": {}}),
        )
        .await;
        event(&mut ws, sentence("半句", false)).await;
        while audio.is_empty() {
            if let Some(Ok(Message::Binary(b))) = ws.next().await {
                audio.extend_from_slice(&b);
            }
        }
        let mut failed = header("task-failed");
        failed["error_code"] = json!("CLIENT_ERROR");
        failed["error_message"] = json!("request timeout after 23 seconds.");
        event(&mut ws, json!({"header": failed, "payload": {}})).await;
        // Keep the socket open until the client goes.
        while ws.next().await.is_some() {}
    });
    let hub = hub();
    let sub = hub.subscribe();
    let mut frames = sub.snapshot;
    let mut live = sub.frames;
    let params = SessionParams {
        settings: settings(&url, &[]),
        ..SessionParams::default()
    };
    let mut session = hub.open(
        anya_engine_dashscope::engine().as_ref(),
        Source::Dictation,
        params,
    );
    assert!(session.send(chunk(1)).await);
    let outcome = tokio::time::timeout(Duration::from_secs(5), session.ended())
        .await
        .expect("ended");
    assert_eq!(outcome.reason, EndReason::Error);
    assert_eq!(outcome.error, Some(ErrorCode::Upstream));
    assert_eq!(
        outcome.detail.as_deref(),
        Some("CLIENT_ERROR: request timeout after 23 seconds.")
    );
    while let Ok(f) = live.try_recv() {
        frames.push(f);
    }
    contract::check(&frames).expect("frames keep the promises");
    assert!(matches!(
        frames.last(),
        Some(Frame::SessionEnd {
            reason: EndReason::Error,
            error: Some(ErrorCode::Upstream),
            ..
        })
    ));
}

#[tokio::test]
async fn task_failed_before_start_is_an_upstream_error() {
    let (url, listener) = listen().await;
    tokio::spawn(async move {
        let mut ws = accept(&listener, Arc::default()).await;
        next_command(&mut ws, &mut Vec::new()).await;
        let mut failed = header("task-failed");
        failed["error_code"] = json!("ModelNotFound");
        failed["error_message"] = json!("Model not found: nope");
        event(&mut ws, json!({"header": failed, "payload": {}})).await;
        while ws.next().await.is_some() {}
    });
    let params = SessionParams {
        settings: settings(&url, &[("model", "nope")]),
        ..SessionParams::default()
    };
    let mut session = anya_engine_dashscope::engine().open(params);
    match session.output.recv().await {
        Some(Output::Error(e)) => assert_eq!(
            e,
            EngineError::Upstream("ModelNotFound: Model not found: nope".into())
        ),
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(session.output.recv().await.is_none());
}

#[tokio::test]
async fn a_refused_handshake_reports_the_http_status() {
    let (url, listener) = listen().await;
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = tcp.read(&mut buf).await.expect("read");
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buf[..n]);
        }
        let body = r#"{"code":"InvalidApiKey","message":"Invalid API-key provided."}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        tcp.write_all(response.as_bytes()).await.expect("write");
        let _ = tcp.shutdown().await;
    });
    let params = SessionParams {
        settings: settings(&url, &[]),
        ..SessionParams::default()
    };
    let mut session = anya_engine_dashscope::engine().open(params);
    match session.output.recv().await {
        Some(Output::Error(EngineError::Upstream(detail))) => {
            assert!(detail.starts_with("HTTP 401"), "{detail}");
            assert!(detail.contains("InvalidApiKey"), "{detail}");
        }
        other => panic!("expected an upstream error, got {other:?}"),
    }
}

#[tokio::test]
async fn dropping_the_input_closes_the_connection() {
    let (url, listener) = listen().await;
    let (closed_tx, closed_rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut ws = accept(&listener, Arc::default()).await;
        next_command(&mut ws, &mut Vec::new()).await;
        event(
            &mut ws,
            json!({"header": header("task-started"), "payload": {}}),
        )
        .await;
        let mut close_frame = false;
        while let Some(Ok(m)) = ws.next().await {
            if let Message::Close(_) = m {
                close_frame = true;
                break;
            }
        }
        let _ = closed_tx.send(close_frame);
    });
    let params = SessionParams {
        settings: settings(&url, &[]),
        ..SessionParams::default()
    };
    let session = anya_engine_dashscope::engine().open(params);
    let mut output = session.output;
    assert!(matches!(output.recv().await, Some(Output::Ready)));
    session
        .input
        .send(Input::Audio(chunk(1)))
        .await
        .expect("send");
    drop(session.input);
    let close_frame = tokio::time::timeout(Duration::from_secs(2), closed_rx)
        .await
        .expect("closed promptly")
        .expect("server reported");
    assert!(close_frame, "a close frame was sent");
    assert!(output.recv().await.is_none(), "abandoning is not an error");
}

#[tokio::test(start_paused = true)]
async fn no_task_started_within_15_s_is_a_connect_error() {
    let (url, listener) = listen().await;
    tokio::spawn(async move {
        let mut ws = accept(&listener, Arc::default()).await;
        while ws.next().await.is_some() {}
    });
    let params = SessionParams {
        settings: settings(&url, &[]),
        ..SessionParams::default()
    };
    let mut session = anya_engine_dashscope::engine().open(params);
    match session.output.recv().await {
        Some(Output::Error(EngineError::Connect(detail))) => {
            assert!(detail.contains("task-started"), "{detail}");
        }
        other => panic!("expected a connect error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_api_key_is_refused() {
    let engine = anya_engine_dashscope::engine();
    let missing = Err(EngineError::NotConfigured("api_key".into()));
    assert_eq!(engine.check(&BTreeMap::new()), missing);
    let blank = BTreeMap::from([("api_key".to_owned(), "  ".to_owned())]);
    assert_eq!(engine.check(&blank), missing);
    let rate = BTreeMap::from([
        ("api_key".to_owned(), "sk-1".to_owned()),
        ("sample_rate".to_owned(), "44100".to_owned()),
    ]);
    assert_eq!(
        engine.check(&rate),
        Err(EngineError::NotConfigured("sample_rate".into()))
    );
    let ok = BTreeMap::from([("api_key".to_owned(), "sk-1".to_owned())]);
    assert_eq!(engine.check(&ok), Ok(()));

    let mut session = engine.open(SessionParams::default());
    match session.output.recv().await {
        Some(Output::Error(e)) => assert_eq!(Err(e), missing),
        other => panic!("expected an error, got {other:?}"),
    }
}

/// Needs a DashScope key and the network:
/// `DASHSCOPE_API_KEY=sk-… cargo test -p anya-engine-dashscope -- --ignored`
#[tokio::test]
#[ignore = "talks to DashScope with a real key"]
async fn the_real_service_finishes_a_quiet_second() {
    let key = std::env::var("DASHSCOPE_API_KEY").expect("DASHSCOPE_API_KEY");
    let hub = hub();
    let params = SessionParams {
        settings: BTreeMap::from([("api_key".to_owned(), key)]),
        ..SessionParams::default()
    };
    let session = hub.open(
        anya_engine_dashscope::engine().as_ref(),
        Source::Dictation,
        params,
    );
    for _ in 0..25 {
        assert!(
            session
                .send(Chunk(vec![0; anya_core::audio::CHUNK_BYTES]))
                .await
        );
    }
    let outcome = session.finish(Duration::from_secs(15)).await;
    assert_eq!(outcome.reason, EndReason::Final, "{outcome:?}");
}
