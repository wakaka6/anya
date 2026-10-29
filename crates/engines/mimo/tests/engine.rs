//! The Xiaomi MiMo engine against a local server.

mod mock;

use std::collections::BTreeMap;
use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{EngineError, Input, Output, SessionParams};
use anya_core::hub::{Hub, HubInfo};
use anya_protocol::{EndReason, ErrorCode, FieldKind, Frame, Source, contract};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use mock::{Reply, serve, unreachable};
use serde_json::{Value, json};

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
    let mut session = anya_engine_mimo::engine().open(params);
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

fn answer(content: &str) -> Reply {
    Reply::json(
        200,
        json!({"id": "c-1", "choices": [{"index": 0, "message": {"role": "assistant", "content": content}}]}),
    )
}

#[test]
fn manifest_matches_the_catalog() {
    let engine = anya_engine_mimo::engine();
    let m = engine.manifest();
    assert_eq!(m.id, "mimo");
    assert!(!m.streaming);
    assert_eq!(m.title.get("zh-CN"), "小米 MiMo");
    assert_eq!(m.title.get("en"), "Xiaomi MiMo");
    assert_eq!(m.blurb.get("zh-CN"), "小米 MiMo 语音识别，整段识别");
    assert_eq!(m.docs.as_deref(), Some("https://platform.xiaomimimo.com/"));
    let keys: Vec<_> = m.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["api_key", "base_url", "model"]);
    assert_eq!(m.fields[0].kind, FieldKind::Secret);
    assert!(m.fields[0].required && !m.fields[0].advanced);
    assert_eq!(m.fields[0].help.get("zh-CN"), "必填，小米 MiMo 控制台");
    assert!(m.fields[1..].iter().all(|f| f.advanced && !f.required));
    assert_eq!(m.fields[2].help.get("zh-CN"), "默认 mimo-v2.5-asr");
    assert_eq!(
        engine.check(&BTreeMap::new()),
        Err(EngineError::NotConfigured("api_key".to_owned()))
    );
}

#[tokio::test]
async fn the_recording_goes_as_chat_audio_and_comes_back_final() {
    let mut server = serve(vec![answer("  你好，MiMo。 ")]).await;
    let out = transcribe(
        params(
            &[
                ("api_key", "mimo-key"),
                ("base_url", &format!("{}/", server.base)),
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
            assert_eq!(r.text, "你好，MiMo。");
            assert_eq!(r.utterances.len(), 1);
            assert!(r.utterances[0].definite);
        }
        other => panic!("expected the final result, got {other:?}"),
    }

    let request = server.next().await;
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.header("api-key"), Some("mimo-key"));
    assert_eq!(request.header("authorization"), None);
    assert!(
        request
            .header("content-type")
            .is_some_and(|t| t.starts_with("application/json"))
    );
    let mut body = request.json();
    let data = body["messages"][0]["content"][0]["input_audio"]["data"]
        .as_str()
        .expect("audio data")
        .to_owned();
    let wav = STANDARD
        .decode(
            data.strip_prefix("data:audio/wav;base64,")
                .expect("a WAV data URL"),
        )
        .expect("valid base64");
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(
        u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
        16_000
    );
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(&wav[44..], &[1, 0, 2, 0, 3, 0]);
    body["messages"][0]["content"][0]["input_audio"]["data"] = Value::Null;
    assert_eq!(
        body,
        json!({
            "model": "mimo-v2.5-asr",
            "messages": [{
                "role": "user",
                "content": [{"type": "input_audio", "input_audio": {"data": null}}],
            }],
            "stream": false,
            "asr_options": {"language": "zh-CN"},
        })
    );
}

#[tokio::test]
async fn the_model_and_configured_language_apply() {
    let mut server = serve(vec![answer("hi")]).await;
    transcribe(
        params(
            &[
                ("api_key", "k"),
                ("base_url", &server.base),
                ("model", "mimo-next"),
                ("language", "en"),
            ],
            None,
        ),
        &[&[0, 0]],
    )
    .await;
    let body = server.next().await.json();
    assert_eq!(body["model"], json!("mimo-next"));
    assert_eq!(body["asr_options"], json!({"language": "en"}));

    let mut server = serve(vec![answer("hi")]).await;
    transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    let body = server.next().await.json();
    assert!(body.get("asr_options").is_none());
}

#[tokio::test]
async fn an_http_error_is_upstream_with_the_servers_message() {
    let server = serve(vec![Reply::json(
        401,
        json!({"error": {"message": "Invalid API Key", "type": "authentication_error"}}),
    )])
    .await;
    let out = transcribe(
        params(&[("api_key", "bad"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("HTTP 401 Unauthorized: Invalid API Key".to_owned())
    );
}

#[tokio::test]
async fn an_error_or_no_choices_in_a_200_answer_is_upstream() {
    let server = serve(vec![Reply::json(
        200,
        json!({"choices": [], "error": {"message": "audio too long"}}),
    )])
    .await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("audio too long".to_owned())
    );

    let server = serve(vec![Reply::json(200, json!({"id": "c-1"}))]).await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("the answer has no choices".to_owned())
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
    let server = serve(vec![answer("hello there")]).await;
    let hub = Hub::new(HubInfo {
        version: "0.0.0".to_owned(),
        session_prefix: "test".to_owned(),
    });
    let mut subscription = hub.subscribe();
    let engine = anya_engine_mimo::engine();
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
            .any(|f| matches!(f, Frame::SessionStart { engine, .. } if engine == "mimo"))
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
