//! Recognition on this computer with whisper.cpp. When the user stops, the
//! whole recording goes as a WAV file to `POST {url}/inference` of a
//! `whisper-server` the user runs; the answer's text is the session's one,
//! final result. Nothing leaves the machine while the address is local.

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

const DEFAULT_URL: &str = "http://127.0.0.1:8080";

/// Whisper writes Simplified Chinese with punctuation more often when the
/// prompt is such a sentence.
const ZH_PROMPT: &str = "以下是普通话的句子。";

/// One request: upload, recognition and answer. A long recording on a big
/// model on a slow machine takes a while.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Most of an answer read.
const ANSWER_LIMIT: usize = 4 << 20;

/// The whisper.cpp engine.
pub fn engine() -> Arc<dyn Engine> {
    Arc::new(WhisperCpp {
        manifest: manifest(),
    })
}

struct WhisperCpp {
    manifest: Manifest,
}

impl Engine for WhisperCpp {
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
    /// Whisper's name for the language: no region, `auto` to detect.
    language: String,
    prompt: String,
}

impl Transcription {
    fn new(params: &SessionParams) -> Result<Transcription, EngineError> {
        let base = params
            .setting("url")
            .map(|u| u.trim().trim_end_matches('/'))
            .filter(|u| !u.is_empty())
            .unwrap_or(DEFAULT_URL);
        let language = language(params.language.as_deref());
        let prompt = [
            if language == "zh" { ZH_PROMPT } else { "" },
            &params.hotwords.join(", "),
        ]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
        Ok(Transcription {
            client: http::client(TIMEOUT)?,
            url: http::endpoint(base, "inference")?,
            language,
            prompt,
        })
    }

    async fn run(self, pcm: Vec<u8>) -> Result<String, EngineError> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        let file = Part::bytes(wav(&pcm, SAMPLE_RATE))
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(http::network)?;
        let mut form = Form::new()
            .part("file", file)
            .text("response_format", "json")
            .text("temperature", "0")
            .text("language", self.language);
        if !self.prompt.is_empty() {
            form = form.text("prompt", self.prompt);
        }
        let response = self
            .client
            .post(self.url)
            .multipart(form)
            .send()
            .await
            .map_err(http::network)?;
        let body = http::body(response, ANSWER_LIMIT).await?;
        let answer: Answer = http::parse(&body)?;
        match answer.error {
            Some(error) => Err(EngineError::Upstream(format!("whisper-server: {error}"))),
            None => Ok(join_segments(&answer.text)),
        }
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    text: String,
    /// whisper-server answers some failures with status 200.
    #[serde(default)]
    error: Option<String>,
}

/// Whisper names languages without a region; `auto` lets it detect one.
fn language(tag: Option<&str>) -> String {
    let lang = tag
        .and_then(|t| t.trim().split(['-', '_']).next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if lang.is_empty() {
        "auto".to_owned()
    } else {
        lang
    }
}

/// Whether `c` is written without spaces between words.
fn is_wide(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{9fff}' | '\u{ff00}'..='\u{ffef}')
}

/// whisper-server puts each segment on a line of its own. Chinese and
/// Japanese run on without a space; other scripts get one.
fn join_segments(text: &str) -> String {
    let mut out = String::new();
    for segment in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let spaced = out.chars().next_back().is_some_and(|last| {
            !is_wide(last) && segment.chars().next().is_some_and(|first| !is_wide(first))
        });
        if spaced {
            out.push(' ');
        }
        out.push_str(segment);
    }
    out
}

fn manifest() -> Manifest {
    Manifest {
        id: "whisper-cpp".to_owned(),
        title: Text::localized([
            ("en", "whisper.cpp (this computer)"),
            ("zh-CN", "whisper.cpp（本机）"),
        ]),
        blurb: Text::localized([
            (
                "en",
                "Recognizes the whole recording with a whisper-server running on this computer; the audio does not leave it",
            ),
            (
                "zh-CN",
                "用本机运行的 whisper-server 识别整段录音，音频不离开这台电脑",
            ),
        ]),
        docs: Some(
            "https://github.com/ggml-org/whisper.cpp/tree/master/examples/server".to_owned(),
        ),
        streaming: false,
        fields: vec![Field {
            key: "url".to_owned(),
            label: Text::localized([("en", "Server address"), ("zh-CN", "服务地址")]),
            help: Text::localized([
                (
                    "en",
                    "Where whisper-server listens; empty is http://127.0.0.1:8080",
                ),
                (
                    "zh-CN",
                    "whisper-server 的地址；留空为 http://127.0.0.1:8080",
                ),
            ]),
            kind: FieldKind::Text,
            required: false,
            advanced: false,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_loses_its_region_and_defaults_to_detection() {
        assert_eq!(language(Some("zh-CN")), "zh");
        assert_eq!(language(Some(" EN_us ")), "en");
        assert_eq!(language(Some("")), "auto");
        assert_eq!(language(None), "auto");
    }

    #[test]
    fn segments_join_with_a_space_only_between_spaced_scripts() {
        assert_eq!(join_segments("你好，\n世界。\n"), "你好，世界。");
        assert_eq!(
            join_segments(" Hello there.\n How are you? "),
            "Hello there. How are you?"
        );
        assert_eq!(join_segments("Hello\n世界"), "Hello世界");
        assert_eq!(join_segments("\n \n"), "");
    }
}
