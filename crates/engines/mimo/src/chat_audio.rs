//! Recognition through a chat-completions endpoint that takes audio input:
//! the WAV goes as an `input_audio` part (a base64 data URL) of one user
//! message to `POST {base_url}/chat/completions`, and the transcript comes
//! back as `choices[0].message.content`.

use anya_core::audio::{SAMPLE_RATE, wav};
use anya_core::engine::EngineError;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::json;

use crate::http;

/// Most of an answer read.
const ANSWER_LIMIT: usize = 8 << 20;

/// One request's endpoint, credential and model.
pub(crate) struct ChatAudio {
    pub(crate) client: Client,
    /// `{base_url}/chat/completions`.
    pub(crate) url: Url,
    /// The header that carries `api_key`, as the service names it.
    pub(crate) auth_header: &'static str,
    pub(crate) api_key: String,
    pub(crate) model: String,
    /// Sent as `asr_options.language` when set.
    pub(crate) language: Option<String>,
}

impl ChatAudio {
    /// The transcript of `pcm`.
    pub(crate) async fn transcribe(self, pcm: &[u8]) -> Result<String, EngineError> {
        let data = format!(
            "data:audio/wav;base64,{}",
            STANDARD.encode(wav(pcm, SAMPLE_RATE))
        );
        let mut body = json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": [{"type": "input_audio", "input_audio": {"data": data}}],
            }],
            "stream": false,
        });
        if let Some(language) = self.language {
            body["asr_options"] = json!({ "language": language });
        }
        let response = self
            .client
            .post(self.url)
            .header(self.auth_header, &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(http::network)?;
        let answer: Answer = http::parse(&http::body(response, ANSWER_LIMIT).await?)?;
        if let Some(message) = answer
            .error
            .and_then(|e| e.message)
            .filter(|m| !m.trim().is_empty())
        {
            return Err(EngineError::Upstream(message.trim().to_owned()));
        }
        answer
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content.unwrap_or_default())
            .ok_or_else(|| EngineError::Upstream("the answer has no choices".to_owned()))
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    choices: Vec<AnswerChoice>,
    #[serde(default)]
    error: Option<AnswerError>,
}

#[derive(Deserialize)]
struct AnswerChoice {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct AnswerError {
    #[serde(default)]
    message: Option<String>,
}
