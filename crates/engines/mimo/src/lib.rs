//! Xiaomi MiMo recognition (`mimo-v2.5-asr`). When the user stops, the whole
//! recording goes to the chat-completions endpoint as audio input; the
//! reply's text is the session's one, final result.

mod chat_audio;
mod http;

use std::sync::Arc;
use std::time::Duration;

use anya_core::engine::{Engine, EngineError, Output, Session, SessionParams, batch_session};
use anya_protocol::{Field, FieldKind, Manifest, Text};
use tokio::sync::mpsc;

use crate::chat_audio::ChatAudio;

const DEFAULT_BASE_URL: &str = "https://api.xiaomimimo.com/v1";
const DEFAULT_MODEL: &str = "mimo-v2.5-asr";

/// One request: upload, recognition and answer.
const TIMEOUT: Duration = Duration::from_secs(120);

/// The engine's logo, shown beside it in the settings.
pub const ICON: &[u8] = include_bytes!("../icon.svg");

/// The Xiaomi MiMo engine.
pub fn engine() -> Arc<dyn Engine> {
    Arc::new(Mimo {
        manifest: manifest(),
    })
}

struct Mimo {
    manifest: Manifest,
}

impl Engine for Mimo {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, params: SessionParams) -> Session {
        match request(&params) {
            Ok(request) => batch_session(move |pcm| async move { request.transcribe(&pcm).await }),
            Err(e) => failed(e),
        }
    }
}

/// A session that failed before it connected.
fn failed(e: EngineError) -> Session {
    let (input, _) = mpsc::channel(1);
    let (output, rx) = mpsc::channel(1);
    let _ = output.try_send(Output::Error(e));
    Session { input, output: rx }
}

/// One session's request, settled when it opens. The key goes raw in an
/// `api-key` header; the language, the session's or else the configured
/// one, goes as given.
fn request(params: &SessionParams) -> Result<ChatAudio, EngineError> {
    let api_key = params
        .setting("api_key")
        .ok_or_else(|| EngineError::NotConfigured("api_key".to_owned()))?;
    let base = params
        .setting("base_url")
        .map(|b| b.trim_end_matches('/'))
        .filter(|b| !b.is_empty())
        .unwrap_or(DEFAULT_BASE_URL);
    let language = params
        .language
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .or_else(|| params.setting("language"));
    Ok(ChatAudio {
        client: http::client(TIMEOUT)?,
        url: http::endpoint(base, "chat/completions")?,
        auth_header: "api-key",
        api_key: api_key.to_owned(),
        model: params.setting("model").unwrap_or(DEFAULT_MODEL).to_owned(),
        language: language.map(str::to_owned),
    })
}

fn manifest() -> Manifest {
    Manifest {
        id: "mimo".to_owned(),
        title: Text::localized([("en", "Xiaomi MiMo"), ("zh-CN", "小米 MiMo")]),
        blurb: Text::localized([
            (
                "en",
                "Xiaomi MiMo speech recognition, recognizes the whole recording",
            ),
            ("zh-CN", "小米 MiMo 语音识别，整段识别"),
        ]),
        docs: Some("https://platform.xiaomimimo.com/".to_owned()),
        streaming: false,
        fields: vec![
            Field {
                key: "api_key".to_owned(),
                label: Text::localized([("en", "API Key"), ("zh-CN", "API Key")]),
                help: Text::localized([
                    ("en", "Required, from the Xiaomi MiMo console"),
                    ("zh-CN", "必填，小米 MiMo 控制台"),
                ]),
                kind: FieldKind::Secret,
                required: true,
                advanced: false,
            },
            Field {
                key: "base_url".to_owned(),
                label: Text::localized([("en", "Base URL"), ("zh-CN", "Base URL")]),
                help: Text::localized([
                    ("en", "Default https://api.xiaomimimo.com/v1"),
                    ("zh-CN", "默认 https://api.xiaomimimo.com/v1"),
                ]),
                kind: FieldKind::Text,
                required: false,
                advanced: true,
            },
            Field {
                key: "model".to_owned(),
                label: Text::localized([("en", "Model"), ("zh-CN", "模型")]),
                help: Text::localized([
                    ("en", "Default mimo-v2.5-asr"),
                    ("zh-CN", "默认 mimo-v2.5-asr"),
                ]),
                kind: FieldKind::Text,
                required: false,
                advanced: true,
            },
        ],
    }
}
