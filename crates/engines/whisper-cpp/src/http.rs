//! HTTP plumbing: a bounded client per session, bounded reads, and failures
//! mapped to [`EngineError`]: the network's to `Connect`, the server's to
//! `Upstream` with its own message.

use std::error::Error as _;
use std::time::Duration;

use anya_core::engine::EngineError;
use reqwest::{Client, Response, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// How long opening a connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Most characters of a server's message kept in an error.
const MESSAGE_CHARS: usize = 300;

/// A client whose every request, upload to last byte, ends within `timeout`.
pub(crate) fn client(timeout: Duration) -> Result<Client, EngineError> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .map_err(network)
}

/// `path` under `base`, an `http` or `https` URL without a trailing `/`.
pub(crate) fn endpoint(base: &str, path: &str) -> Result<Url, EngineError> {
    let invalid = |why: String| EngineError::Connect(format!("invalid url {base:?}: {why}"));
    let url = Url::parse(&format!("{base}/{path}")).map_err(|e| invalid(e.to_string()))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(invalid(format!("scheme {other:?} is not http or https"))),
    }
}

/// A request that failed on the way: connecting, sending, timing out.
pub(crate) fn network(e: reqwest::Error) -> EngineError {
    let e = e.without_url();
    let mut detail = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        detail.push_str(": ");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    EngineError::Connect(detail)
}

/// The body of a successful response, at most `limit` bytes. Any other
/// status is [`EngineError::Upstream`] with the server's message.
pub(crate) async fn body(mut response: Response, limit: usize) -> Result<Vec<u8>, EngineError> {
    let status = response.status();
    let mut body = Vec::new();
    let mut whole = true;
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        let room = limit - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            whole = false;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let message = message(&body);
        return Err(EngineError::Upstream(if message.is_empty() {
            format!("HTTP {status}")
        } else {
            format!("HTTP {status}: {message}")
        }));
    }
    if !whole {
        return Err(EngineError::Upstream(format!(
            "the answer is larger than {limit} bytes"
        )));
    }
    Ok(body)
}

/// A successful body as `T`.
pub(crate) fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, EngineError> {
    serde_json::from_slice(body)
        .map_err(|e| EngineError::Upstream(format!("unexpected answer: {e}")))
}

/// What an error body says: the message field of the usual JSON error
/// shapes, else the body itself, shortened.
fn message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let json = serde_json::from_str::<Value>(&text).ok();
    let field = json.as_ref().and_then(|v| {
        let error = v.get("error");
        error
            .and_then(|e| e.get("message"))
            .or(error)
            .or_else(|| v.get("message"))
            .or_else(|| v.get("error_message"))
            .or_else(|| v.get("detail"))
            .and_then(Value::as_str)
    });
    shorten(field.unwrap_or(&text).trim())
}

fn shorten(s: &str) -> String {
    match s.char_indices().nth(MESSAGE_CHARS) {
        Some((end, _)) => format!("{}…", &s[..end]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_prefers_the_json_error_text() {
        assert_eq!(message(br#"{"error":{"message":"bad key"}}"#), "bad key");
        assert_eq!(message(br#"{"error":"bad key"}"#), "bad key");
        assert_eq!(message(br#"{"message":"bad key"}"#), "bad key");
        assert_eq!(message(b"  gateway down \n"), "gateway down");
        assert_eq!(message(b""), "");
    }

    #[test]
    fn long_messages_are_cut_on_a_character() {
        let long = "错".repeat(MESSAGE_CHARS + 5);
        let cut = message(long.as_bytes());
        assert_eq!(cut.chars().count(), MESSAGE_CHARS + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn endpoint_takes_only_http_urls() {
        let url = endpoint("http://127.0.0.1:8080/v1", "audio/transcriptions");
        assert_eq!(
            url.map(|u| u.to_string()),
            Ok("http://127.0.0.1:8080/v1/audio/transcriptions".to_owned())
        );
        assert!(matches!(
            endpoint("api.example.com/v1", "x"),
            Err(EngineError::Connect(_))
        ));
        assert!(matches!(
            endpoint("ftp://example.com", "x"),
            Err(EngineError::Connect(_))
        ));
    }
}
