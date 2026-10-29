//! OpenAI-compatible transcription. When the user stops, the whole
//! recording goes as a WAV file to `POST {base_url}/audio/transcriptions`;
//! the answer's text is the session's one, final result. Any server that
//! speaks this API works: OpenAI itself, Groq, a local whisper.cpp or
//! faster-whisper server.

mod http;

use std::sync::Arc;
use std::time::Duration;

use anya_core::audio::{SAMPLE_RATE, wav};
use anya_core::engine::{Engine, EngineError, Output, Session, SessionParams, batch_session};
use anya_protocol::{Field, FieldKind, Manifest, Text};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, Url};
use serde::Deserialize;
use tokio::sync::mpsc;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "whisper-1";

/// One request: upload, recognition and answer.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Most of an answer read.
const ANSWER_LIMIT: usize = 4 << 20;

/// The OpenAI-compatible engine.
pub fn engine() -> Arc<dyn Engine> {
    Arc::new(OpenAi {
        manifest: manifest(),
    })
}

struct OpenAi {
    manifest: Manifest,
}

impl Engine for OpenAi {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, params: SessionParams) -> Session {
        match Transcription::new(&params) {
            Ok(t) => batch_session(move |pcm| t.run(pcm)),
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

/// One session's request, settled when it opens.
struct Transcription {
    client: Client,
    url: Url,
    api_key: String,
    model: String,
    language: Option<String>,
}

impl Transcription {
    fn new(params: &SessionParams) -> Result<Transcription, EngineError> {
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
        Ok(Transcription {
            client: http::client(TIMEOUT)?,
            url: http::endpoint(base, "audio/transcriptions")?,
            api_key: api_key.to_owned(),
            model: params.setting("model").unwrap_or(DEFAULT_MODEL).to_owned(),
            language: language.map(str::to_owned),
        })
    }

    async fn run(self, pcm: Vec<u8>) -> Result<String, EngineError> {
        let file = Part::bytes(wav(&pcm, SAMPLE_RATE))
            .file_name("recording.wav")
            .mime_str("audio/wav")
            .map_err(http::network)?;
        let mut form = Form::new()
            .part("file", file)
            .text("model", self.model)
            .text("response_format", "json");
        if let Some(language) = self.language {
            form = form.text("language", language);
        }
        let response = self
            .client
            .post(self.url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(http::network)?;
        let body = http::body(response, ANSWER_LIMIT).await?;
        let answer: Answer = http::parse(&body)?;
        Ok(answer.text)
    }
}

#[derive(Deserialize)]
struct Answer {
    text: String,
}

fn manifest() -> Manifest {
    Manifest {
        id: "openai".to_owned(),
        title: Text::localized([("en", "OpenAI Whisper"), ("zh-CN", "OpenAI Whisper")]),
        blurb: Text::localized([
            (
                "en",
                "OpenAI-compatible Whisper API, recognizes the whole recording; can point at a local or third-party endpoint",
            ),
            (
                "zh-CN",
                "OpenAI 兼容 Whisper 接口，整段识别；可指向本地/第三方端点",
            ),
        ]),
        docs: Some("https://platform.openai.com/docs/api-reference/audio".to_owned()),
        streaming: false,
        fields: vec![
            Field {
                key: "api_key".to_owned(),
                label: Text::localized([("en", "API Key"), ("zh-CN", "API Key")]),
                help: Text::localized([
                    ("en", "Required, Bearer token"),
                    ("zh-CN", "必填，Bearer token"),
                ]),
                kind: FieldKind::Secret,
                required: true,
                advanced: false,
            },
            Field {
                key: "base_url".to_owned(),
                label: Text::localized([("en", "Base URL"), ("zh-CN", "Base URL")]),
                help: Text::localized([
                    (
                        "en",
                        "Default https://api.openai.com/v1; can point at a local whisper.cpp server",
                    ),
                    (
                        "zh-CN",
                        "默认 https://api.openai.com/v1，可指本地 whisper.cpp server",
                    ),
                ]),
                kind: FieldKind::Text,
                required: false,
                advanced: true,
            },
            Field {
                key: "model".to_owned(),
                label: Text::localized([("en", "Model"), ("zh-CN", "模型")]),
                help: Text::localized([("en", "Default whisper-1"), ("zh-CN", "默认 whisper-1")]),
                kind: FieldKind::Text,
                required: false,
                advanced: true,
            },
        ],
    }
}
