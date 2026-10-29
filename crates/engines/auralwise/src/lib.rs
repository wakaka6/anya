//! AuralWise transcription through its asynchronous task API. When the user
//! stops, the recording is submitted as a task (`POST {base_url}/tasks`, the
//! WAV in base64), the task is polled until it is done, and its result's
//! text is the session's one, final result.

mod http;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anya_core::audio::{SAMPLE_RATE, wav};
use anya_core::engine::{Engine, EngineError, Output, Session, SessionParams, batch_session};
use anya_protocol::{Field, FieldKind, Manifest, Text};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Client, RequestBuilder, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;

const DEFAULT_BASE_URL: &str = "https://api.auralwise.cn/v1";
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(3);
const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MIN_POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// One request; the whole task is bounded by `poll_timeout`.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Most of each answer read.
const CREATE_LIMIT: usize = 2 << 20;
const STATUS_LIMIT: usize = 1 << 20;
const RESULT_LIMIT: usize = 8 << 20;

/// The AuralWise engine.
pub fn engine() -> Arc<dyn Engine> {
    Arc::new(AuralWise {
        manifest: manifest(),
    })
}

struct AuralWise {
    manifest: Manifest,
}

impl Engine for AuralWise {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Requires `api_key`, and `optimize` to be `auto`, `true` or `false`.
    fn check(&self, settings: &BTreeMap<String, String>) -> Result<(), EngineError> {
        let params = SessionParams {
            settings: settings.clone(),
            ..SessionParams::default()
        };
        Config::read(&params).map(drop)
    }

    fn open(&self, params: SessionParams) -> Session {
        match Config::read(&params).and_then(Task::new) {
            Ok(task) => batch_session(move |pcm| task.run(pcm)),
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

/// The settings a session opens with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Config {
    api_key: String,
    base_url: String,
    /// ISO 639-1, as the service takes it: `zh-CN` gives `zh`.
    language: Option<String>,
    /// `None` lets the service choose by language.
    optimize: Option<bool>,
    diarize: bool,
    events: bool,
    poll_interval: Duration,
    poll_timeout: Duration,
}

impl Config {
    fn read(params: &SessionParams) -> Result<Config, EngineError> {
        let api_key = params
            .setting("api_key")
            .ok_or_else(|| EngineError::NotConfigured("api_key".to_owned()))?;
        let optimize = match params
            .setting("optimize")
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("auto") => None,
            Some("true") => Some(true),
            Some("false") => Some(false),
            Some(_) => return Err(EngineError::NotConfigured("optimize".to_owned())),
        };
        let language = params
            .language
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .or_else(|| params.setting("language"))
            .and_then(|l| l.split(['-', '_']).next())
            .filter(|l| !l.is_empty())
            .map(str::to_ascii_lowercase);
        let events = params
            .setting("enable_events")
            .or_else(|| params.setting("enable_audio_events"));
        Ok(Config {
            api_key: api_key.to_owned(),
            base_url: params
                .setting("base_url")
                .map(|b| b.trim_end_matches('/'))
                .filter(|b| !b.is_empty())
                .unwrap_or(DEFAULT_BASE_URL)
                .to_owned(),
            language,
            optimize,
            diarize: flag(params.setting("enable_diarize"), false),
            events: flag(events, false),
            poll_interval: duration(params.setting("poll_interval"), DEFAULT_POLL_INTERVAL)
                .max(MIN_POLL_INTERVAL),
            poll_timeout: duration(params.setting("poll_timeout"), DEFAULT_POLL_TIMEOUT)
                .max(MIN_POLL_TIMEOUT),
        })
    }
}

/// `1/true/yes/on` or `0/false/no/off`, any case; anything else is `default`.
fn flag(value: Option<&str>, default: bool) -> bool {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        _ => default,
    }
}

/// A duration such as `3s`, `1m30s` or `500ms`; unparsable or zero is `default`.
fn duration(value: Option<&str>, default: Duration) -> Duration {
    value
        .and_then(parse_duration)
        .filter(|d| !d.is_zero())
        .unwrap_or(default)
}

fn parse_duration(s: &str) -> Option<Duration> {
    let mut rest = s.trim();
    if rest.is_empty() {
        return None;
    }
    let mut seconds = 0.0;
    while !rest.is_empty() {
        let number = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let n: f64 = rest[..number].parse().ok()?;
        rest = &rest[number..];
        let unit = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let scale = match &rest[..unit] {
            "ns" => 1e-9,
            "us" | "µs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        seconds += n * scale;
        rest = &rest[unit..];
    }
    Duration::try_from_secs_f64(seconds).ok()
}

/// One session's task, settled when it opens.
struct Task {
    client: Client,
    tasks: Url,
    api_key: String,
    options: Value,
    poll_interval: Duration,
    poll_timeout: Duration,
}

impl Task {
    fn new(config: Config) -> Result<Task, EngineError> {
        let mut options = json!({
            "enable_asr": true,
            "enable_diarize": config.diarize,
            "enable_audio_events": config.events,
        });
        if let Some(language) = config.language {
            options["asr_language"] = json!(language);
        }
        if let Some(optimize) = config.optimize {
            options["optimize"] = json!(optimize);
        }
        Ok(Task {
            client: http::client(TIMEOUT)?,
            tasks: http::endpoint(&config.base_url, "tasks")?,
            api_key: config.api_key,
            options,
            poll_interval: config.poll_interval,
            poll_timeout: config.poll_timeout,
        })
    }

    async fn run(self, pcm: Vec<u8>) -> Result<String, EngineError> {
        let id = self.create(&pcm).await?;
        self.wait(&id).await?;
        let result: Value = self
            .fetch(
                self.client.get(self.task_url(&id, Some("result"))?),
                RESULT_LIMIT,
            )
            .await?;
        Ok(transcript(&result))
    }

    async fn create(&self, pcm: &[u8]) -> Result<String, EngineError> {
        #[derive(Deserialize)]
        struct Created {
            #[serde(default)]
            id: String,
        }
        let body = json!({
            "options": self.options,
            "allow_training": false,
            "audio_base64": STANDARD.encode(wav(pcm, SAMPLE_RATE)),
            "audio_filename": "recording.wav",
        });
        let created: Created = self
            .fetch(
                self.client.post(self.tasks.clone()).json(&body),
                CREATE_LIMIT,
            )
            .await?;
        if created.id.is_empty() {
            return Err(EngineError::Upstream(
                "the answer has no task id".to_owned(),
            ));
        }
        Ok(created.id)
    }

    /// Polls the task until it is done, failed, or out of time.
    async fn wait(&self, id: &str) -> Result<(), EngineError> {
        #[derive(Deserialize)]
        struct Status {
            #[serde(default)]
            status: String,
            #[serde(default)]
            error_message: Option<String>,
        }
        let url = self.task_url(id, None)?;
        let deadline = Instant::now() + self.poll_timeout;
        loop {
            let s: Status = self
                .fetch(self.client.get(url.clone()), STATUS_LIMIT)
                .await?;
            match s.status.as_str() {
                "done" => return Ok(()),
                "failed" | "abandoned" => {
                    let message = s
                        .error_message
                        .filter(|m| !m.trim().is_empty())
                        .unwrap_or_else(|| s.status.clone());
                    return Err(EngineError::Upstream(format!(
                        "task {}: {}",
                        s.status,
                        message.trim()
                    )));
                }
                _ => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(EngineError::Upstream(format!(
                    "task {id} not done after {:?}; status {:?}",
                    self.poll_timeout, s.status
                )));
            }
            tokio::time::sleep(self.poll_interval.min(deadline - now)).await;
        }
    }

    /// `{base_url}/tasks/{id}`, then `/{tail}`.
    fn task_url(&self, id: &str, tail: Option<&str>) -> Result<Url, EngineError> {
        let mut url = self.tasks.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| EngineError::Connect(format!("invalid base_url {}", self.tasks)))?;
            path.push(id);
            if let Some(tail) = tail {
                path.push(tail);
            }
        }
        Ok(url)
    }

    async fn fetch<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        limit: usize,
    ) -> Result<T, EngineError> {
        let response = request
            .header("X-API-Key", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(http::network)?;
        http::parse(&http::body(response, limit).await?)
    }
}

/// The result's segments joined, a space only between two Latin letters or
/// digits; the top-level `text` when there are none.
fn transcript(result: &Value) -> String {
    let mut text = String::new();
    let segments = result.get("segments").and_then(Value::as_array);
    for segment in segments.into_iter().flatten() {
        let Some(t) = segment.get("text").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if t.is_empty() {
            continue;
        }
        let latin = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
        if latin(text.chars().next_back()) && latin(t.chars().next()) {
            text.push(' ');
        }
        text.push_str(t);
    }
    if text.is_empty()
        && let Some(t) = result.get("text").and_then(Value::as_str)
    {
        text = t.trim().to_owned();
    }
    text
}

fn manifest() -> Manifest {
    Manifest {
        id: "auralwise".to_owned(),
        title: Text::localized([("en", "AuralWise"), ("zh-CN", "AuralWise")]),
        blurb: Text::localized([
            (
                "en",
                "AuralWise asynchronous task transcription; the text appears once the whole recording is recognized",
            ),
            ("zh-CN", "AuralWise 异步任务转写，整段识别后上屏"),
        ]),
        docs: Some("https://auralwise.cn/".to_owned()),
        streaming: false,
        fields: vec![
            Field {
                key: "api_key".to_owned(),
                label: Text::localized([("en", "API Key"), ("zh-CN", "API Key")]),
                help: Text::localized([
                    (
                        "en",
                        "Required; create one (asr_…) in your auralwise.cn account settings",
                    ),
                    ("zh-CN", "必填，auralwise.cn 账号设置里创建（asr_…）"),
                ]),
                kind: FieldKind::Secret,
                required: true,
                advanced: false,
            },
            Field {
                key: "base_url".to_owned(),
                label: Text::localized([("en", "Base URL"), ("zh-CN", "Base URL")]),
                help: Text::localized([
                    ("en", "Default https://api.auralwise.cn/v1"),
                    ("zh-CN", "默认 https://api.auralwise.cn/v1"),
                ]),
                kind: FieldKind::Text,
                required: false,
                advanced: true,
            },
            Field {
                key: "optimize".to_owned(),
                label: Text::localized([("en", "Optimize"), ("zh-CN", "Optimize")]),
                help: Text::localized([
                    (
                        "en",
                        "auto chooses by language / true costs less / false gives high quality with word timestamps",
                    ),
                    (
                        "zh-CN",
                        "auto 按语言自动 / true 省成本 / false 高质量词级时间戳",
                    ),
                ]),
                kind: FieldKind::Choice {
                    options: vec!["auto".to_owned(), "true".to_owned(), "false".to_owned()],
                },
                required: false,
                advanced: true,
            },
        ],
    }
}

#[cfg(test)]
#[path = "../tests/mock/mod.rs"]
mod mock;

#[cfg(test)]
mod tests {
    use super::*;
    use mock::{Reply, serve};

    fn config(pairs: &[(&str, &str)], language: Option<&str>) -> Result<Config, EngineError> {
        Config::read(&SessionParams {
            language: language.map(str::to_owned),
            settings: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..SessionParams::default()
        })
    }

    #[test]
    fn settings_take_their_defaults() {
        assert_eq!(
            config(&[("api_key", " asr_k ")], None),
            Ok(Config {
                api_key: "asr_k".to_owned(),
                base_url: DEFAULT_BASE_URL.to_owned(),
                language: None,
                optimize: None,
                diarize: false,
                events: false,
                poll_interval: DEFAULT_POLL_INTERVAL,
                poll_timeout: DEFAULT_POLL_TIMEOUT,
            })
        );
    }

    #[test]
    fn settings_are_read_and_clamped() {
        let c = config(
            &[
                ("api_key", "k"),
                ("base_url", "http://127.0.0.1:9/v1/"),
                ("language", "en-US"),
                ("optimize", "TRUE"),
                ("enable_diarize", "yes"),
                ("enable_audio_events", "on"),
                ("poll_interval", "200ms"),
                ("poll_timeout", "2s"),
            ],
            None,
        )
        .expect("a config");
        assert_eq!(c.base_url, "http://127.0.0.1:9/v1");
        assert_eq!(c.language.as_deref(), Some("en"));
        assert_eq!(c.optimize, Some(true));
        assert!(c.diarize && c.events);
        assert_eq!(c.poll_interval, MIN_POLL_INTERVAL);
        assert_eq!(c.poll_timeout, MIN_POLL_TIMEOUT);

        let c = config(
            &[
                ("api_key", "k"),
                ("language", "en"),
                ("optimize", "false"),
                ("poll_timeout", "1m30s"),
            ],
            Some("zh_CN"),
        )
        .expect("a config");
        assert_eq!(
            c.language.as_deref(),
            Some("zh"),
            "the session's language wins"
        );
        assert_eq!(c.optimize, Some(false));
        assert_eq!(c.poll_timeout, Duration::from_secs(90));
    }

    #[test]
    fn missing_key_or_bad_optimize_is_not_configured() {
        assert_eq!(
            config(&[], None),
            Err(EngineError::NotConfigured("api_key".to_owned()))
        );
        assert_eq!(
            config(&[("api_key", "k"), ("optimize", "fast")], None),
            Err(EngineError::NotConfigured("optimize".to_owned()))
        );
        let settings = BTreeMap::from([
            ("api_key".to_owned(), "k".to_owned()),
            ("optimize".to_owned(), "fast".to_owned()),
        ]);
        assert_eq!(
            engine().check(&settings),
            Err(EngineError::NotConfigured("optimize".to_owned()))
        );
    }

    #[test]
    fn durations_parse_like_config_values() {
        assert_eq!(parse_duration("3s"), Some(Duration::from_secs(3)));
        assert_eq!(parse_duration("1m30s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("1.5h"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_duration("10"), None);
        assert_eq!(parse_duration("s"), None);
        assert_eq!(parse_duration("3 days"), None);
        assert_eq!(
            duration(Some("0s"), DEFAULT_POLL_INTERVAL),
            DEFAULT_POLL_INTERVAL
        );
        assert_eq!(
            duration(Some("soon"), DEFAULT_POLL_INTERVAL),
            DEFAULT_POLL_INTERVAL
        );
    }

    #[test]
    fn segments_join_with_spaces_only_between_latin_words() {
        let result = json!({"segments": [
            {"text": " 你好 "}, {"text": "世界"}, {"text": "Hello"}, {"text": "world."},
            {"text": ""}, {"start": 1.0}, "noise", {"text": "再见"}
        ], "text": "ignored"});
        assert_eq!(transcript(&result), "你好世界Hello world.再见");
        assert_eq!(
            transcript(&json!({"segments": [], "text": " only "})),
            "only"
        );
        assert_eq!(transcript(&json!({})), "");
    }

    #[tokio::test]
    async fn polling_stops_at_the_deadline() {
        let mut server = serve(vec![Reply::json(200, json!({"status": "processing"}))]).await;
        let mut task = Task::new(
            config(&[("api_key", "k"), ("base_url", &server.base)], None).expect("a config"),
        )
        .expect("a task");
        task.poll_interval = Duration::from_millis(20);
        task.poll_timeout = Duration::from_millis(100);
        let err = task.wait("t-1").await.expect_err("out of time");
        assert_eq!(
            err,
            EngineError::Upstream(
                "task t-1 not done after 100ms; status \"processing\"".to_owned()
            )
        );
        let mut polls = 0;
        while let Ok(r) = server.requests.try_recv() {
            assert_eq!(r.path, "/v1/tasks/t-1");
            polls += 1;
        }
        assert!(polls >= 3, "polled {polls} times");
    }
}
