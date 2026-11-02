//! The whisper.cpp engine against a local server.

mod mock;

use std::collections::BTreeMap;
use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{EngineError, Input, Output, SessionParams};
use anya_core::hub::{Hub, HubInfo};
use anya_protocol::{EndReason, ErrorCode, FieldKind, Frame, Source, contract};
use mock::{Reply, find, serve, unreachable};
use serde_json::json;

fn params(pairs: &[(&str, &str)], language: Option<&str>, hotwords: &[&str]) -> SessionParams {
    SessionParams {
        language: language.map(str::to_owned),
        hotwords: hotwords.iter().map(|w| (*w).to_owned()).collect(),
        settings: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// Opens a session, sends `chunks` and Finish, and returns every output.
async fn transcribe(params: SessionParams, chunks: &[&[u8]]) -> Vec<Output> {
    let mut session = anya_engine_whisper_cpp::engine().open(params);
    for c in chunks {
        let _ = session.input.send(Input::Audio(Chunk(c.to_vec()))).await;
    }
    let _ = session.input.send(Input::Finish).await;
    let mut out = Vec::new();
    while let Some(o) = tokio::time::timeout(Duration::from_secs(10), session.output.recv())
        .await
        .expect("the session ends in time")
    {
        out.push(o);
    }
    out
}

fn the_error(out: &[Output]) -> EngineError {
    match out.last() {
        Some(Output::Error(e)) => e.clone(),
        other => panic!("expected an error, got {other:?}"),
    }
}

/// Multipart parts as (headers, data).
fn multipart(content_type: &str, body: &[u8]) -> Vec<(String, Vec<u8>)> {
    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .expect("a boundary")
        .trim_matches('"');
    let delimiter = format!("--{boundary}");
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = find(rest, delimiter.as_bytes()) {
        rest = &rest[start + delimiter.len()..];
        if rest.starts_with(b"--") {
            break;
        }
        let Some(end) = find(rest, delimiter.as_bytes()) else {
            break;
        };
        let part = &rest[2..end - 2];
        let split = find(part, b"\r\n\r\n").expect("part headers");
        out.push((
            String::from_utf8_lossy(&part[..split]).into_owned(),
            part[split + 4..].to_vec(),
        ));
    }
    out
}

fn text_field(parts: &[(String, Vec<u8>)], name: &str) -> Option<String> {
    let wanted = format!("name=\"{name}\"");
    parts
        .iter()
        .find(|(h, _)| h.contains(&wanted))
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
}

/// The server's base address, the mock's `/v1` dropped.
fn base(server_base: &str) -> String {
    server_base.trim_end_matches("/v1").to_owned()
}

#[test]
fn the_manifest_needs_no_setting_and_names_the_address() {
    let engine = anya_engine_whisper_cpp::engine();
    let m = engine.manifest();
    assert_eq!(m.id, "whisper-cpp");
    assert!(!m.streaming);
    assert_eq!(m.title.get("zh-CN"), "whisper.cpp（本机）");
    assert!(!m.blurb.get("en").is_empty());
    assert_eq!(m.fields.len(), 1);
    assert_eq!(m.fields[0].key, "url");
    assert_eq!(m.fields[0].kind, FieldKind::Text);
    assert!(!m.fields[0].required);
    assert_eq!(engine.check(&BTreeMap::new()), Ok(()));
}

#[tokio::test]
async fn the_recording_goes_as_a_wav_and_the_segments_come_back_as_one_final_text() {
    let mut server = serve(vec![Reply::json(200, json!({"text": "你好，\n世界。\n"}))]).await;
    let out = transcribe(
        params(
            &[("url", &format!("{}/", base(&server.base)))],
            Some("zh-CN"),
            &["Anya", "Rust"],
        ),
        &[&[1, 0, 2, 0], &[3, 0]],
    )
    .await;

    assert!(matches!(out.first(), Some(Output::Ready)));
    assert_eq!(out.len(), 2, "{out:?}");
    match &out[1] {
        Output::Result(r) => {
            assert!(r.is_final);
            assert_eq!(r.text, "你好，世界。");
            assert!(r.utterances[0].definite);
        }
        other => panic!("expected the final result, got {other:?}"),
    }

    let request = server.next().await;
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/inference");
    let parts = multipart(
        request.header("content-type").expect("a type"),
        &request.body,
    );
    let (_, wav) = parts
        .iter()
        .find(|(h, _)| h.contains("name=\"file\""))
        .expect("a file part");
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[44..], &[1, 0, 2, 0, 3, 0]);
    assert_eq!(
        text_field(&parts, "response_format").as_deref(),
        Some("json")
    );
    assert_eq!(text_field(&parts, "temperature").as_deref(), Some("0"));
    assert_eq!(text_field(&parts, "language").as_deref(), Some("zh"));
    assert_eq!(
        text_field(&parts, "prompt").as_deref(),
        Some("以下是普通话的句子。 Anya, Rust")
    );
}

#[tokio::test]
async fn without_a_language_whisper_detects_and_without_hotwords_there_is_no_prompt() {
    let mut server = serve(vec![Reply::json(200, json!({"text": "hi"}))]).await;
    transcribe(
        params(&[("url", &base(&server.base))], None, &[]),
        &[&[0, 0]],
    )
    .await;
    let request = server.next().await;
    let parts = multipart(request.header("content-type").unwrap_or(""), &request.body);
    assert_eq!(text_field(&parts, "language").as_deref(), Some("auto"));
    assert_eq!(text_field(&parts, "prompt"), None);
}

#[tokio::test]
async fn a_silent_session_is_an_empty_final_result_without_a_request() {
    let mut server = serve(vec![Reply::json(200, json!({"text": "never"}))]).await;
    let out = transcribe(params(&[("url", &base(&server.base))], None, &[]), &[]).await;
    match out.last() {
        Some(Output::Result(r)) => assert!(r.is_final && r.text.is_empty()),
        other => panic!("expected an empty final result, got {other:?}"),
    }
    assert!(server.requests.try_recv().is_err());
}

#[tokio::test]
async fn a_failure_the_server_reports_is_upstream() {
    let server = serve(vec![Reply::json(
        200,
        json!({"error": "failed to read audio"}),
    )])
    .await;
    let out = transcribe(
        params(&[("url", &base(&server.base))], None, &[]),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("whisper-server: failed to read audio".to_owned())
    );

    let server = serve(vec![Reply::json(500, json!({"error": "busy"}))]).await;
    let out = transcribe(
        params(&[("url", &base(&server.base))], None, &[]),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("HTTP 500 Internal Server Error: busy".to_owned())
    );
}

#[tokio::test]
async fn a_server_that_is_not_running_is_connect_and_a_bad_address_fails_before_ready() {
    let out = transcribe(
        params(&[("url", &base(&unreachable().await))], None, &[]),
        &[&[0, 0]],
    )
    .await;
    assert!(matches!(out.first(), Some(Output::Ready)));
    assert!(
        matches!(the_error(&out), EngineError::Connect(_)),
        "{out:?}"
    );

    let out = transcribe(params(&[("url", "localhost:8080")], None, &[]), &[&[0, 0]]).await;
    assert_eq!(out.len(), 1);
    assert!(matches!(the_error(&out), EngineError::Connect(_)));
}

#[tokio::test]
async fn a_session_through_the_hub_keeps_the_protocol() {
    let server = serve(vec![Reply::json(200, json!({"text": "hello there"}))]).await;
    let hub = Hub::new(HubInfo {
        version: "0.0.0".to_owned(),
        session_prefix: "test".to_owned(),
    });
    let mut subscription = hub.subscribe();
    let engine = anya_engine_whisper_cpp::engine();
    let session = hub.open(
        engine.as_ref(),
        Source::Dictation,
        params(&[("url", &base(&server.base))], None, &[]),
    );
    assert!(session.send(Chunk(vec![0; 640])).await);
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(outcome.reason, EndReason::Final);
    assert_eq!(outcome.text, "hello there");

    let mut frames = subscription.snapshot;
    while let Ok(f) = subscription.frames.try_recv() {
        frames.push(f);
    }
    contract::check(&frames).expect("frames keep the protocol's promises");
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, Frame::SessionStart { engine, .. } if engine == "whisper-cpp"))
    );

    let session = hub.open(
        engine.as_ref(),
        Source::Dictation,
        params(&[("url", &base(&unreachable().await))], None, &[]),
    );
    assert!(session.send(Chunk(vec![0; 640])).await);
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(outcome.reason, EndReason::Error);
    assert_eq!(outcome.error, Some(ErrorCode::Connect));
}
