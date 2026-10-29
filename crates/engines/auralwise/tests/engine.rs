//! The AuralWise engine against a local server.

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
    let mut session = anya_engine_auralwise::engine().open(params);
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

fn the_text(out: &[Output]) -> String {
    assert!(matches!(out.first(), Some(Output::Ready)), "{out:?}");
    assert_eq!(out.len(), 2, "{out:?}");
    match &out[1] {
        Output::Result(r) => {
            assert!(r.is_final);
            r.text.clone()
        }
        other => panic!("expected the final result, got {other:?}"),
    }
}

fn created() -> Reply {
    Reply::json(200, json!({"id": "t-1", "status": "pending"}))
}

#[test]
fn manifest_matches_the_catalog() {
    let engine = anya_engine_auralwise::engine();
    let m = engine.manifest();
    assert_eq!(m.id, "auralwise");
    assert!(!m.streaming);
    assert_eq!(m.title.get("zh-CN"), "AuralWise");
    assert_eq!(
        m.blurb.get("zh-CN"),
        "AuralWise 异步任务转写，整段识别后上屏"
    );
    assert!(!m.blurb.get("en").is_empty());
    assert_eq!(m.docs.as_deref(), Some("https://auralwise.cn/"));
    let keys: Vec<_> = m.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["api_key", "base_url", "optimize"]);
    assert_eq!(m.fields[0].kind, FieldKind::Secret);
    assert!(m.fields[0].required && !m.fields[0].advanced);
    assert_eq!(
        m.fields[0].help.get("zh-CN"),
        "必填，auralwise.cn 账号设置里创建（asr_…）"
    );
    assert!(m.fields[1..].iter().all(|f| f.advanced && !f.required));
    assert_eq!(
        m.fields[2].kind,
        FieldKind::Choice {
            options: vec!["auto".to_owned(), "true".to_owned(), "false".to_owned()]
        }
    );
    assert_eq!(
        engine.check(&BTreeMap::new()),
        Err(EngineError::NotConfigured("api_key".to_owned()))
    );
}

#[tokio::test]
async fn a_task_is_created_polled_until_done_and_its_result_read() {
    let mut server = serve(vec![
        created(),
        Reply::json(200, json!({"id": "t-1", "status": "processing"})),
        Reply::json(200, json!({"id": "t-1", "status": "done"})),
        Reply::json(
            200,
            json!({"language": "zh", "language_probability": 0.98, "segments": [
                {"start": 0.0, "end": 1.0, "text": " 你好 "},
                {"start": 1.0, "end": 2.0, "text": "Anya"},
                {"start": 2.0, "end": 3.0, "text": "speaking"}
            ]}),
        ),
    ])
    .await;
    let out = transcribe(
        params(
            &[
                ("api_key", "asr_test"),
                ("base_url", &format!("{}/", server.base)),
                ("optimize", "true"),
                ("poll_interval", "1s"),
            ],
            Some("zh-CN"),
        ),
        &[&[1, 0, 2, 0], &[3, 0]],
    )
    .await;
    assert_eq!(the_text(&out), "你好Anya speaking");

    let create = server.next().await;
    assert_eq!(create.method, "POST");
    assert_eq!(create.path, "/v1/tasks");
    assert_eq!(create.header("x-api-key"), Some("asr_test"));
    assert_eq!(create.header("accept"), Some("application/json"));
    assert!(
        create
            .header("content-type")
            .is_some_and(|t| t.starts_with("application/json"))
    );
    let body = create.json();
    assert_eq!(
        body["options"],
        json!({
            "enable_asr": true,
            "enable_diarize": false,
            "enable_audio_events": false,
            "asr_language": "zh",
            "optimize": true,
        })
    );
    assert_eq!(body["allow_training"], json!(false));
    assert_eq!(body["audio_filename"], json!("recording.wav"));
    let wav = STANDARD
        .decode(body["audio_base64"].as_str().expect("base64 audio"))
        .expect("valid base64");
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(
        u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
        16_000
    );
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(&wav[44..], &[1, 0, 2, 0, 3, 0]);

    for _ in 0..2 {
        let poll = server.next().await;
        assert_eq!(
            (poll.method.as_str(), poll.path.as_str()),
            ("GET", "/v1/tasks/t-1")
        );
        assert_eq!(poll.header("x-api-key"), Some("asr_test"));
    }
    let result = server.next().await;
    assert_eq!(
        (result.method.as_str(), result.path.as_str()),
        ("GET", "/v1/tasks/t-1/result")
    );
    assert_eq!(result.header("x-api-key"), Some("asr_test"));
}

#[tokio::test]
async fn auto_optimize_is_left_out_and_top_level_text_is_the_fallback() {
    let mut server = serve(vec![
        created(),
        Reply::json(200, json!({"status": "done"})),
        Reply::json(200, json!({"text": " plain text "})),
    ])
    .await;
    let out = transcribe(
        params(
            &[
                ("api_key", "k"),
                ("base_url", &server.base),
                ("enable_diarize", "true"),
                ("enable_events", "1"),
            ],
            None,
        ),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(the_text(&out), "plain text");
    let body = server.next().await.json();
    assert_eq!(
        body["options"],
        json!({"enable_asr": true, "enable_diarize": true, "enable_audio_events": true})
    );
}

#[tokio::test]
async fn a_failed_task_is_upstream_with_its_message() {
    let server = serve(vec![
        created(),
        Reply::json(
            200,
            json!({"status": "failed", "error_message": "audio could not be decoded"}),
        ),
    ])
    .await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("task failed: audio could not be decoded".to_owned())
    );

    let server = serve(vec![
        created(),
        Reply::json(200, json!({"status": "abandoned"})),
    ])
    .await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("task abandoned: abandoned".to_owned())
    );
}

#[tokio::test]
async fn an_http_error_is_upstream_with_the_servers_message() {
    let server = serve(vec![Reply::json(401, json!({"detail": "invalid API key"}))]).await;
    let out = transcribe(
        params(&[("api_key", "bad"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("HTTP 401 Unauthorized: invalid API key".to_owned())
    );

    let server = serve(vec![
        created(),
        Reply::json(200, json!({"status": "done"})),
        Reply::json(500, json!({"message": "result store down"})),
    ])
    .await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("HTTP 500 Internal Server Error: result store down".to_owned())
    );
}

#[tokio::test]
async fn an_answer_without_a_task_id_is_upstream() {
    let server = serve(vec![Reply::json(200, json!({"status": "pending"}))]).await;
    let out = transcribe(
        params(&[("api_key", "k"), ("base_url", &server.base)], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(
        the_error(&out),
        EngineError::Upstream("the answer has no task id".to_owned())
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
async fn bad_settings_fail_before_ready() {
    let out = transcribe(params(&[], None), &[&[0, 0]]).await;
    assert_eq!(out.len(), 1);
    assert_eq!(
        the_error(&out),
        EngineError::NotConfigured("api_key".to_owned())
    );

    let out = transcribe(
        params(&[("api_key", "k"), ("optimize", "fast")], None),
        &[&[0, 0]],
    )
    .await;
    assert_eq!(out.len(), 1);
    assert_eq!(
        the_error(&out),
        EngineError::NotConfigured("optimize".to_owned())
    );
}

#[tokio::test]
async fn a_session_through_the_hub_keeps_the_protocol() {
    let server = serve(vec![
        created(),
        Reply::json(200, json!({"status": "done"})),
        Reply::json(
            200,
            json!({"segments": [{"text": "hello"}, {"text": "there"}]}),
        ),
    ])
    .await;
    let hub = Hub::new(HubInfo {
        version: "0.0.0".to_owned(),
        session_prefix: "test".to_owned(),
    });
    let mut subscription = hub.subscribe();
    let engine = anya_engine_auralwise::engine();
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
            .any(|f| matches!(f, Frame::SessionStart { engine, .. } if engine == "auralwise"))
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
