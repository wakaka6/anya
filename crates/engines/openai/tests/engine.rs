//! The OpenAI-compatible engine against a local server.

mod mock;

use std::collections::BTreeMap;
use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{EngineError, Input, Output, SessionParams};
use anya_core::hub::{Hub, HubInfo};
use anya_protocol::{EndReason, ErrorCode, FieldKind, Frame, Source, contract};
use mock::{Reply, Server, find, serve, unreachable};
use serde_json::json;

fn params(pairs: &[(&str, &str)], language: Option<&str>) -> SessionParams {
    SessionParams {
        language: language.map(str::to_owned),
        hotwords: vec!["Anya".to_owned()],
        settings: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// Opens a session, sends `chunks` and Finish, and returns every output.
async fn transcribe(params: SessionParams, chunks: &[&[u8]]) -> Vec<Output> {
    let mut session = anya_engine_openai::engine().open(params);
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

fn field<'a>(parts: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a (String, Vec<u8>)> {
    let wanted = format!("name=\"{name}\"");
    parts.iter().find(|(h, _)| h.contains(&wanted))
}

fn text_field(parts: &[(String, Vec<u8>)], name: &str) -> Option<String> {
    field(parts, name).map(|(_, d)| String::from_utf8_lossy(d).into_owned())
}

#[test]
fn manifest_matches_the_catalog() {
    let engine = anya_engine_openai::engine();
    let m = engine.manifest();
    assert_eq!(m.id, "openai");
    assert!(!m.streaming);
    assert_eq!(m.title.get("zh-CN"), "OpenAI Whisper");
    assert_eq!(
        m.blurb.get("zh-CN"),
        "OpenAI 兼容 Whisper 接口，整段识别；可指向本地/第三方端点"
    );
    assert!(!m.blurb.get("en").is_empty());
    assert_eq!(
        m.docs.as_deref(),
        Some("https://platform.openai.com/docs/api-reference/audio")
    );
    let keys: Vec<_> = m.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["api_key", "base_url", "model"]);
    assert_eq!(m.fields[0].kind, FieldKind::Secret);
    assert!(m.fields[0].required && !m.fields[0].advanced);
    assert!(m.fields[1..].iter().all(|f| f.advanced && !f.required));
    assert_eq!(m.fields[2].label.get("zh-CN"), "模型");
    assert_eq!(m.fields[2].help.get("en"), "Default whisper-1");
    assert_eq!(
        engine.check(&BTreeMap::new()),
        Err(EngineError::NotConfigured("api_key".to_owned()))
    );
}

#[tokio::test]
async fn the_recording_goes_as_a_wav_upload_and_comes_back_final() {
    let mut server = serve(vec![Reply::json(200, json!({"text": "  你好，世界  "}))]).await;
    let out = transcribe(
        params(
            &[
                ("api_key", "sk-test"),
                ("base_url", &format!("{}/", server.base)),
                ("model", "whisper-large-v3"),
            ],
            Some("zh-CN"),
        ),
        &[&[1, 0, 2, 0], &[3, 0]],
    )
    .await;

    assert!(matches!(out.first(), Some(Output::Ready)));
    assert_eq!(out.len(), 2, "{out:?}");
    match &out[1] {
        Output::Result(r) => {
            assert!(r.is_final);
            assert_eq!(r.text, "你好，世界");
            assert_eq!(r.utterances.len(), 1);
            assert!(r.utterances[0].definite);
        }
        other => panic!("expected the final result, got {other:?}"),
    }

    let request = server.next().await;
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/audio/transcriptions");
    assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    let content_type = request.header("content-type").expect("a content type");
    assert!(content_type.starts_with("multipart/form-data"));
    let parts = multipart(content_type, &request.body);
    let (file_headers, wav) = field(&parts, "file").expect("a file part");
    assert!(file_headers.contains("filename=\"recording.wav\""));
    assert!(file_headers.contains("audio/wav"));
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(
        u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
        16_000
    );
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 6);
    assert_eq!(&wav[44..], &[1, 0, 2, 0, 3, 0]);
    assert_eq!(
        text_field(&parts, "model").as_deref(),
        Some("whisper-large-v3")
    );
    assert_eq!(
        text_field(&parts, "response_format").as_deref(),
        Some("json")
    );
    assert_eq!(text_field(&parts, "language").as_deref(), Some("zh-CN"));
    assert!(field(&parts, "prompt").is_none(), "hotwords are not sent");
}

#[tokio::test]
async fn defaults_and_the_configured_language_apply() {
    let mut server = serve(vec![Reply::json(200, json!({"text": "hi"}))]).await;
    transcribe(
        params(
            &[
                ("api_key", "k"),
                ("base_url", &server.base),
                ("language", "en"),
            ],
            None,
        ),
        &[&[0, 0]],
    )
    .await;
    let request = server.next().await;
    let parts = multipart(request.header("content-type").unwrap_or(""), &request.body);
    assert_eq!(text_field(&parts, "model").as_deref(), Some("whisper-1"));
    assert_eq!(text_field(&parts, "language").as_deref(), Some("en"));

    let mut server = serve(vec![Reply::json(200, json!({"text": "hi"}))]).await;
    transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    let request = server.next().await;
    let parts = multipart(request.header("content-type").unwrap_or(""), &request.body);
    assert!(field(&parts, "language").is_none());
}

#[tokio::test]
async fn an_http_error_is_upstream_with_the_servers_message() {
    let server = serve(vec![Reply::json(
        401,
        json!({"error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}}),
    )])
    .await;
    let out = transcribe(
        params(&[("api_key", "bad"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("HTTP 401 Unauthorized: Incorrect API key provided".to_owned())
    );
}

#[tokio::test]
async fn an_unexpected_answer_is_upstream() {
    let server = serve(vec![Reply {
        status: 200,
        body: "<html>proxy</html>".to_owned(),
    }])
    .await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert!(
        matches!(the_error(&out), EngineError::Upstream(d) if d.starts_with("unexpected answer"))
    );
}

#[tokio::test]
async fn an_unreachable_server_is_connect() {
    let base = unreachable().await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &base)], None),
        &[&[0, 0]],
    )
    .await;
    assert!(matches!(out.first(), Some(Output::Ready)));
    assert!(
        matches!(the_error(&out), EngineError::Connect(_)),
        "{out:?}"
    );
}

#[tokio::test]
async fn a_missing_key_fails_before_ready() {
    let out = transcribe(params(&[], None), &[&[0, 0]]).await;
    assert_eq!(out.len(), 1);
    assert_eq!(
        the_error(&out),
        EngineError::NotConfigured("api_key".to_owned())
    );
}

#[tokio::test]
async fn a_session_through_the_hub_keeps_the_protocol() {
    let server: Server = serve(vec![Reply::json(200, json!({"text": "hello there"}))]).await;
    let hub = Hub::new(HubInfo {
        version: "0.0.0".to_owned(),
        session_prefix: "test".to_owned(),
    });
    let mut subscription = hub.subscribe();
    let engine = anya_engine_openai::engine();
    let session = hub.open(
        engine.as_ref(),
        Source::Dictation,
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
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
            .any(|f| matches!(f, Frame::SessionStart { engine, .. } if engine == "openai"))
    );

    let session = hub.open(
        engine.as_ref(),
        Source::Dictation,
        params(
            &[("api_key", "k"), ("base_url", &unreachable().await)],
            None,
        ),
    );
    assert!(session.send(Chunk(vec![0; 640])).await);
    let outcome = session.finish(Duration::from_secs(10)).await;
    assert_eq!(outcome.reason, EndReason::Error);
    assert_eq!(outcome.error, Some(ErrorCode::Connect));
}
