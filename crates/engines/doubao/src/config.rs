//! The engine's settings, read from a session's params.

use std::collections::BTreeMap;

use anya_core::engine::EngineError;
use serde_json::{Map, Value, json};

const DEFAULT_HOST: &str = "openspeech.bytedance.com";

/// Context tables built in the console's self-learning platform, sent by
/// name or id when set.
const TABLES: [&str; 6] = [
    "boosting_table_name",
    "boosting_table_id",
    "correct_table_name",
    "correct_table_id",
    "regex_correct_table_name",
    "regex_correct_table_id",
];

/// How the service is reached and authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Auth {
    /// New console: one API key.
    ApiKey(String),
    /// Legacy console: an app id and an access token.
    Legacy { app_id: String, access_key: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamMode {
    /// Bidirectional streaming that answers when the text changes; allows
    /// the second pass.
    Async,
    /// Streaming input, answered after 15 s of audio or at the end.
    NoStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) auth: Auth,
    /// `X-Api-Resource-Id`.
    pub(crate) resource_id: String,
    pub(crate) mode: StreamMode,
    /// `scheme://host`, without a trailing slash.
    origin: String,
    enable_itn: bool,
    enable_punc: bool,
    enable_ddc: bool,
    enable_lid: bool,
    enable_nonstream: bool,
    tables: Vec<(&'static str, String)>,
    language: Option<String>,
}

fn get<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    settings
        .get(key)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
}

/// `1/true/yes/on` or `0/false/no/off`, any case; anything else is `default`.
fn flag(settings: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
    match get(settings, key).map(str::to_ascii_lowercase).as_deref() {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        _ => default,
    }
}

/// A choice setting: unset gives `default`, an unknown value is an error
/// naming the key.
fn choice<'a>(
    settings: &'a BTreeMap<String, String>,
    key: &str,
    options: &[&str],
    default: &'a str,
) -> Result<&'a str, EngineError> {
    match get(settings, key) {
        None => Ok(default),
        Some(v) if options.contains(&v) => Ok(v),
        Some(_) => Err(EngineError::Invalid(key.to_owned())),
    }
}

impl Config {
    /// Reads the settings. Credentials are an API key alone, or an app id
    /// with an access key; a missing one is named by its key.
    pub(crate) fn parse(
        settings: &BTreeMap<String, String>,
        language: Option<&str>,
    ) -> Result<Config, EngineError> {
        let missing = |key: &str| EngineError::NotConfigured(key.to_owned());
        let auth = match (
            get(settings, "api_key"),
            get(settings, "app_id"),
            get(settings, "access_key"),
        ) {
            (Some(key), ..) => Auth::ApiKey(key.to_owned()),
            (None, Some(app_id), Some(access_key)) => Auth::Legacy {
                app_id: app_id.to_owned(),
                access_key: access_key.to_owned(),
            },
            (None, Some(_), None) => return Err(missing("access_key")),
            (None, None, Some(_)) => return Err(missing("app_id")),
            (None, None, None) => return Err(missing("api_key")),
        };
        // New-console keys are usually bound to 2.0 (seedasr), legacy
        // credentials to 1.0 (bigasr).
        let default_version = match auth {
            Auth::ApiKey(_) => "2.0",
            Auth::Legacy { .. } => "1.0",
        };
        let version = choice(settings, "model_version", &["1.0", "2.0"], default_version)?;
        let billing = choice(
            settings,
            "stream_billing",
            &["duration", "concurrent"],
            "duration",
        )?;
        let mode = match choice(settings, "stream_mode", &["async", "nostream"], "async")? {
            "nostream" => StreamMode::NoStream,
            _ => StreamMode::Async,
        };
        let resource_id = match get(settings, "resource_id") {
            Some(id) => id.to_owned(),
            None => {
                let model = if version == "1.0" {
                    "bigasr"
                } else {
                    "seedasr"
                };
                format!("volc.{model}.sauc.{billing}")
            }
        };
        let host = get(settings, "base_host").unwrap_or(DEFAULT_HOST);
        let origin = if host.contains("://") {
            host.trim_end_matches('/').to_owned()
        } else {
            format!("wss://{}", host.trim_end_matches('/'))
        };
        Ok(Config {
            auth,
            resource_id,
            mode,
            origin,
            enable_itn: flag(settings, "enable_itn", true),
            enable_punc: flag(settings, "enable_punc", true),
            enable_ddc: flag(settings, "enable_ddc", false),
            enable_lid: flag(settings, "enable_lid", false),
            enable_nonstream: flag(settings, "enable_nonstream", true),
            tables: TABLES
                .iter()
                .filter_map(|&k| get(settings, k).map(|v| (k, v.to_owned())))
                .collect(),
            language: language
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .or_else(|| get(settings, "language"))
                .map(str::to_owned),
        })
    }

    /// The streaming endpoint.
    pub(crate) fn url(&self) -> String {
        let path = match self.mode {
            StreamMode::Async => "/api/v3/sauc/bigmodel_async",
            StreamMode::NoStream => "/api/v3/sauc/bigmodel_nostream",
        };
        format!("{}{path}", self.origin)
    }

    /// The full client request: audio format, recognition options, hotwords.
    pub(crate) fn request(&self, hotwords: &[String]) -> Value {
        let mut request = Map::new();
        request.insert("model_name".into(), json!("bigmodel"));
        request.insert("enable_itn".into(), json!(self.enable_itn));
        request.insert("enable_punc".into(), json!(self.enable_punc));
        request.insert("enable_ddc".into(), json!(self.enable_ddc));
        request.insert("enable_lid".into(), json!(self.enable_lid));
        request.insert("enable_word".into(), json!(false));
        // The second pass runs only on the bidirectional endpoint.
        request.insert(
            "enable_nonstream".into(),
            json!(self.enable_nonstream && self.mode == StreamMode::Async),
        );
        request.insert("result_type".into(), json!("full"));
        request.insert("show_utterances".into(), json!(true));
        for (key, value) in &self.tables {
            request.insert((*key).into(), json!(value));
        }
        if let Some(context) = hotword_context(hotwords) {
            request.insert("corpus".into(), json!({ "context": context }));
        }
        let mut audio = json!({
            "format": "pcm",
            "rate": anya_core::audio::SAMPLE_RATE,
            "bits": 16,
            "channel": 1,
            "codec": "raw",
        });
        if let (Some(lang), Some(map)) = (&self.language, audio.as_object_mut()) {
            map.insert("language".into(), json!(lang));
        }
        json!({
            "user": { "uid": "anya" },
            "audio": audio,
            "request": request,
        })
    }
}

/// Hotwords as the service's context: a JSON string of
/// `{"hotwords":[{"word":…}]}`, trimmed and without repeats; `None` when no
/// word remains.
fn hotword_context(words: &[String]) -> Option<String> {
    let mut seen = Vec::new();
    for word in words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
        if !seen.contains(&word) {
            seen.push(word);
        }
    }
    if seen.is_empty() {
        return None;
    }
    let hotwords: Vec<Value> = seen.iter().map(|w| json!({ "word": w })).collect();
    Some(json!({ "hotwords": hotwords }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn parse(pairs: &[(&str, &str)]) -> Result<Config, EngineError> {
        Config::parse(&settings(pairs), None)
    }

    #[test]
    fn credentials_are_an_api_key_or_an_app_id_with_an_access_key() {
        let missing = |k: &str| Err(EngineError::NotConfigured(k.to_owned()));
        assert_eq!(parse(&[]).map(|c| c.auth), missing("api_key"));
        assert_eq!(
            parse(&[("api_key", "  ")]).map(|c| c.auth),
            missing("api_key")
        );
        assert_eq!(
            parse(&[("app_id", "a")]).map(|c| c.auth),
            missing("access_key")
        );
        assert_eq!(
            parse(&[("access_key", "s")]).map(|c| c.auth),
            missing("app_id")
        );
        assert_eq!(
            parse(&[("api_key", " k "), ("app_id", "a")]).map(|c| c.auth),
            Ok(Auth::ApiKey("k".into()))
        );
        assert_eq!(
            parse(&[("app_id", "a"), ("access_key", "s")]).map(|c| c.auth),
            Ok(Auth::Legacy {
                app_id: "a".into(),
                access_key: "s".into()
            })
        );
    }

    #[test]
    fn the_resource_follows_credentials_version_and_billing() {
        let id = |pairs: &[(&str, &str)]| parse(pairs).map(|c| c.resource_id);
        assert_eq!(
            id(&[("api_key", "k")]),
            Ok("volc.seedasr.sauc.duration".into())
        );
        assert_eq!(
            id(&[("app_id", "a"), ("access_key", "s")]),
            Ok("volc.bigasr.sauc.duration".into())
        );
        assert_eq!(
            id(&[
                ("api_key", "k"),
                ("model_version", "1.0"),
                ("stream_billing", "concurrent")
            ]),
            Ok("volc.bigasr.sauc.concurrent".into())
        );
        assert_eq!(
            id(&[("api_key", "k"), ("resource_id", "volc.custom")]),
            Ok("volc.custom".into())
        );
    }

    #[test]
    fn unknown_choices_name_their_setting() {
        for key in ["model_version", "stream_billing", "stream_mode"] {
            assert_eq!(
                parse(&[("api_key", "k"), (key, "stream")]),
                Err(EngineError::Invalid(key.to_owned()))
            );
        }
    }

    #[test]
    fn the_endpoint_follows_mode_and_host() {
        let url = |pairs: &[(&str, &str)]| parse(pairs).map(|c| c.url());
        assert_eq!(
            url(&[("api_key", "k")]),
            Ok("wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async".into())
        );
        assert_eq!(
            url(&[
                ("api_key", "k"),
                ("stream_mode", "nostream"),
                ("base_host", "gw.example/")
            ]),
            Ok("wss://gw.example/api/v3/sauc/bigmodel_nostream".into())
        );
        assert_eq!(
            url(&[("api_key", "k"), ("base_host", "ws://127.0.0.1:9000")]),
            Ok("ws://127.0.0.1:9000/api/v3/sauc/bigmodel_async".into())
        );
    }

    #[test]
    fn the_request_carries_options_tables_language_and_hotwords() {
        let c = parse(&[
            ("api_key", "k"),
            ("enable_lid", "TRUE"),
            ("enable_punc", "off"),
            ("enable_itn", "maybe"),
            ("boosting_table_name", "tbl"),
            ("correct_table_id", " cid "),
            ("language", "en-US"),
        ]);
        let words = ["", "Wayland", "Wayland", " wtype "].map(String::from);
        let req = c.map(|c| c.request(&words)).unwrap_or_default();
        assert_eq!(req["user"]["uid"], "anya");
        assert_eq!(
            req["audio"],
            json!({"format": "pcm", "rate": 16000, "bits": 16, "channel": 1,
                   "codec": "raw", "language": "en-US"})
        );
        let r = &req["request"];
        assert_eq!(r["model_name"], "bigmodel");
        assert_eq!(r["enable_lid"], true);
        assert_eq!(r["enable_punc"], false);
        assert_eq!(r["enable_itn"], true);
        assert_eq!(r["enable_ddc"], false);
        assert_eq!(r["enable_nonstream"], true);
        assert_eq!(r["result_type"], "full");
        assert_eq!(r["show_utterances"], true);
        assert_eq!(r["boosting_table_name"], "tbl");
        assert_eq!(r["correct_table_id"], "cid");
        assert!(r.get("boosting_table_id").is_none());
        assert_eq!(
            r["corpus"]["context"],
            r#"{"hotwords":[{"word":"Wayland"},{"word":"wtype"}]}"#
        );
    }

    #[test]
    fn the_session_language_wins_and_nostream_has_no_second_pass() {
        let c = Config::parse(
            &settings(&[
                ("api_key", "k"),
                ("language", "en-US"),
                ("stream_mode", "nostream"),
            ]),
            Some("zh-CN"),
        );
        let req = c.map(|c| c.request(&[])).unwrap_or_default();
        assert_eq!(req["audio"]["language"], "zh-CN");
        assert_eq!(req["request"]["enable_nonstream"], false);
        assert!(req["request"].get("corpus").is_none());
    }
}
