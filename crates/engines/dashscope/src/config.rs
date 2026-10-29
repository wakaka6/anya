//! The engine's settings, read and checked once per session.

use std::collections::BTreeMap;

use anya_core::engine::EngineError;

const DEFAULT_MODEL: &str = "fun-asr-realtime";
const DEFAULT_URL: &str = "wss://dashscope.aliyuncs.com/api-ws/v1/inference/";

/// What a session connects with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) api_key: String,
    pub(crate) model: String,
    pub(crate) url: String,
    /// 16000, or 8000 for models that take nothing else.
    pub(crate) sample_rate: u32,
    /// BCP-47 tag used when the session names no language.
    pub(crate) language: Option<String>,
}

impl Config {
    /// Reads `settings`; fails on a missing `api_key` or a `sample_rate`
    /// other than 8000 or 16000.
    pub(crate) fn from_settings(
        settings: &BTreeMap<String, String>,
    ) -> Result<Config, EngineError> {
        let value = |key: &str| {
            settings
                .get(key)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
        };
        let api_key = value("api_key")
            .ok_or_else(|| EngineError::NotConfigured("api_key".to_owned()))?
            .to_owned();
        let model = value("model").unwrap_or(DEFAULT_MODEL).to_owned();
        let sample_rate = match value("sample_rate") {
            None if model.contains("8k") => 8000,
            None => 16000,
            Some("8000") => 8000,
            Some("16000") => 16000,
            Some(_) => return Err(EngineError::NotConfigured("sample_rate".to_owned())),
        };
        let url = match value("base_url") {
            Some(url) => url.trim_end_matches('/').to_owned(),
            None => DEFAULT_URL.to_owned(),
        };
        Ok(Config {
            api_key,
            model,
            url,
            sample_rate,
            language: value("language").map(str::to_owned),
        })
    }
}

/// The `language_hints` entry for a BCP-47 tag: its language, lowercased
/// (`zh-CN` gives `zh`). `None` leaves the language to the service.
pub(crate) fn language_hint(tag: &str) -> Option<String> {
    let tag = tag.trim();
    let lang = match tag.find(['-', '_']) {
        Some(i) if i > 0 => &tag[..i],
        _ => tag,
    };
    (!lang.is_empty()).then(|| lang.to_ascii_lowercase())
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

    #[test]
    fn defaults_fill_everything_but_the_key() {
        let cfg = Config::from_settings(&settings(&[("api_key", " sk-1 ")])).unwrap();
        assert_eq!(
            cfg,
            Config {
                api_key: "sk-1".into(),
                model: "fun-asr-realtime".into(),
                url: "wss://dashscope.aliyuncs.com/api-ws/v1/inference/".into(),
                sample_rate: 16000,
                language: None,
            }
        );
    }

    #[test]
    fn eight_k_models_default_to_8000() {
        let cfg = Config::from_settings(&settings(&[
            ("api_key", "sk-1"),
            ("model", "fun-asr-flash-8k-realtime"),
        ]))
        .unwrap();
        assert_eq!(cfg.sample_rate, 8000);
        let forced = Config::from_settings(&settings(&[
            ("api_key", "sk-1"),
            ("model", "fun-asr-flash-8k-realtime"),
            ("sample_rate", "16000"),
        ]))
        .unwrap();
        assert_eq!(forced.sample_rate, 16000);
    }

    #[test]
    fn base_url_and_language_are_read() {
        let cfg = Config::from_settings(&settings(&[
            ("api_key", "sk-1"),
            ("base_url", "ws://127.0.0.1:9/ws//"),
            ("language", "en-US"),
        ]))
        .unwrap();
        assert_eq!(cfg.url, "ws://127.0.0.1:9/ws");
        assert_eq!(cfg.language.as_deref(), Some("en-US"));
    }

    #[test]
    fn missing_key_and_bad_rate_are_refused() {
        let no_key = Config::from_settings(&settings(&[("api_key", "  ")]));
        assert_eq!(no_key, Err(EngineError::NotConfigured("api_key".into())));
        for rate in ["44100", "8k", "0"] {
            let bad =
                Config::from_settings(&settings(&[("api_key", "sk-1"), ("sample_rate", rate)]));
            assert_eq!(bad, Err(EngineError::NotConfigured("sample_rate".into())));
        }
    }

    #[test]
    fn language_hint_is_the_language_subtag() {
        assert_eq!(language_hint("zh-CN").as_deref(), Some("zh"));
        assert_eq!(language_hint("EN_us").as_deref(), Some("en"));
        assert_eq!(language_hint("ja").as_deref(), Some("ja"));
        assert_eq!(language_hint(" "), None);
    }
}
