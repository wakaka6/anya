//! DashScope's WebSocket inference API: the commands sent as text frames and
//! the events received.

use serde::Deserialize;
use serde_json::json;

/// Starts recognition of `format: pcm` audio at `sample_rate`.
pub(crate) fn run_task(task_id: &str, model: &str, sample_rate: u32, hint: Option<&str>) -> String {
    let mut parameters = serde_json::Map::new();
    parameters.insert("format".to_owned(), json!("pcm"));
    parameters.insert("sample_rate".to_owned(), json!(sample_rate));
    if let Some(hint) = hint {
        parameters.insert("language_hints".to_owned(), json!([hint]));
    }
    json!({
        "header": {"action": "run-task", "task_id": task_id, "streaming": "duplex"},
        "payload": {
            "task_group": "audio",
            "task": "asr",
            "function": "recognition",
            "model": model,
            "parameters": parameters,
            "input": {},
        },
    })
    .to_string()
}

/// No more audio: the service sends its last results, then `task-finished`.
pub(crate) fn finish_task(task_id: &str) -> String {
    json!({
        "header": {"action": "finish-task", "task_id": task_id, "streaming": "duplex"},
        "payload": {"input": {}},
    })
    .to_string()
}

/// A server event that matters to a session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// `task-started`: audio may follow.
    Started,
    /// A `result-generated` sentence, heartbeats left out.
    Sentence { text: String, end: bool },
    /// `task-finished`: every result has been sent.
    Finished,
    /// `task-failed`, with the service's code and message.
    Failed(String),
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    header: Header,
    #[serde(default)]
    payload: Option<Payload>,
}

#[derive(Default, Deserialize)]
struct Header {
    #[serde(default)]
    event: String,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    output: Option<Output>,
}

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    sentence: Option<Sentence>,
}

#[derive(Deserialize)]
struct Sentence {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    heartbeat: Option<bool>,
    #[serde(default)]
    sentence_end: Option<bool>,
}

/// Reads one text frame. `None` for anything a session ignores: events it
/// does not know, heartbeats, and text that is not an event.
pub(crate) fn parse(frame: &str) -> Option<Event> {
    let raw: Raw = match serde_json::from_str(frame) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::debug!(error = %e, "ignoring a DashScope message that is not an event");
            return None;
        }
    };
    match raw.header.event.as_str() {
        "task-started" => Some(Event::Started),
        "task-finished" => Some(Event::Finished),
        "task-failed" => {
            let code = raw.header.error_code.unwrap_or_default();
            let message = raw.header.error_message.unwrap_or_default();
            Some(Event::Failed(match (code.is_empty(), message.is_empty()) {
                (false, false) => format!("{code}: {message}"),
                (true, false) => message,
                (false, true) => code,
                (true, true) => "task-failed".to_owned(),
            }))
        }
        "result-generated" => {
            let sentence = raw.payload?.output?.sentence?;
            if sentence.heartbeat == Some(true) {
                return None;
            }
            Some(Event::Sentence {
                text: sentence.text.unwrap_or_default(),
                end: sentence.sentence_end == Some(true),
            })
        }
        other => {
            tracing::debug!(event = other, "ignoring a DashScope event");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn value(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn run_task_shape() {
        let sent = value(&run_task("t-1", "fun-asr-realtime", 16000, Some("zh")));
        assert_eq!(
            sent,
            json!({
                "header": {"action": "run-task", "task_id": "t-1", "streaming": "duplex"},
                "payload": {
                    "task_group": "audio",
                    "task": "asr",
                    "function": "recognition",
                    "model": "fun-asr-realtime",
                    "parameters": {"format": "pcm", "sample_rate": 16000, "language_hints": ["zh"]},
                    "input": {},
                },
            })
        );
        let auto = value(&run_task("t-1", "fun-asr-flash-8k-realtime", 8000, None));
        assert_eq!(
            auto["payload"]["parameters"],
            json!({"format": "pcm", "sample_rate": 8000})
        );
    }

    #[test]
    fn finish_task_shape() {
        assert_eq!(
            value(&finish_task("t-1")),
            json!({
                "header": {"action": "finish-task", "task_id": "t-1", "streaming": "duplex"},
                "payload": {"input": {}},
            })
        );
    }

    #[test]
    fn server_events() {
        assert_eq!(
            parse(
                r#"{"header":{"task_id":"t-1","event":"task-started","attributes":{}},"payload":{}}"#
            ),
            Some(Event::Started)
        );
        let result = r#"{"header":{"task_id":"t-1","event":"result-generated","attributes":{}},
            "payload":{"output":{"sentence":{"begin_time":170,"end_time":null,"text":"好，我知道了",
            "heartbeat":false,"sentence_end":true,"words":[{"begin_time":170,"end_time":295,
            "text":"好","punctuation":"，"}]}},"usage":{"duration":3}}}"#;
        assert_eq!(
            parse(result),
            Some(Event::Sentence {
                text: "好，我知道了".into(),
                end: true
            })
        );
        let partial = r#"{"header":{"event":"result-generated"},
            "payload":{"output":{"sentence":{"text":"好","sentence_end":false}},"usage":null}}"#;
        assert_eq!(
            parse(partial),
            Some(Event::Sentence {
                text: "好".into(),
                end: false
            })
        );
        let heartbeat = r#"{"header":{"event":"result-generated"},
            "payload":{"output":{"sentence":{"text":"","heartbeat":true,"sentence_end":false}}}}"#;
        assert_eq!(parse(heartbeat), None);
        assert_eq!(
            parse(
                r#"{"header":{"task_id":"t-1","event":"task-finished","attributes":{}},"payload":{"output":{},"usage":null}}"#
            ),
            Some(Event::Finished)
        );
        assert_eq!(
            parse(
                r#"{"header":{"task_id":"t-1","event":"task-failed","error_code":"CLIENT_ERROR","error_message":"request timeout after 23 seconds.","attributes":{}},"payload":{}}"#
            ),
            Some(Event::Failed(
                "CLIENT_ERROR: request timeout after 23 seconds.".into()
            ))
        );
        assert_eq!(
            parse(r#"{"header":{"event":"task-failed","error_message":"bad model"}}"#),
            Some(Event::Failed("bad model".into()))
        );
        assert_eq!(parse(r#"{"header":{"event":"something-new"}}"#), None);
        assert_eq!(parse("not json"), None);
    }
}
