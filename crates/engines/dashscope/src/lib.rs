//! DashScope Fun-ASR realtime recognition.
//!
//! One WebSocket per session to DashScope's inference endpoint: a `run-task`
//! command starts the task, audio follows as binary PCM frames, `finish-task`
//! ends it, and the service answers with `result-generated` events until
//! `task-finished` or `task-failed`. A sentence the service has ended
//! (`sentence_end`) is a definite utterance; the sentence still being spoken
//! is not.
//!
//! Settings: `api_key` (required), `model` (default `fun-asr-realtime`) and
//! `sample_rate` (`16000` or `8000`; unset picks 8000 for models whose name
//! contains `8k`, which only take 8 kHz, and 16000 otherwise). Audio is
//! halved to 8 kHz inside the engine when 8000 is in use. Two settings have no
//! field: `base_url` points at a proxy or gateway instead of DashScope, and
//! `language` is the language hint used when the session names none. Fun-ASR
//! takes hotwords only as a vocabulary created beforehand through another
//! API, so session hotwords are not sent.

mod config;
mod resample;
mod session;
mod transcript;
mod wire;

use std::collections::BTreeMap;
use std::sync::Arc;

use anya_core::engine::{Engine, EngineError, Session, SessionParams};
use anya_protocol::{Field, FieldKind, Manifest, Text};

use crate::config::Config;

/// The engine's logo, shown beside it in the settings.
pub const ICON: &[u8] = include_bytes!("../icon.svg");

/// The DashScope Fun-ASR engine.
pub fn engine() -> Arc<dyn Engine> {
    Arc::new(DashScope {
        manifest: manifest(),
    })
}

struct DashScope {
    manifest: Manifest,
}

impl Engine for DashScope {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn check(&self, settings: &BTreeMap<String, String>) -> Result<(), EngineError> {
        Config::from_settings(settings).map(drop)
    }

    fn open(&self, params: SessionParams) -> Session {
        session::open(params)
    }
}

fn manifest() -> Manifest {
    Manifest {
        id: "dashscope".to_owned(),
        title: Text::localized([("en", "Qwen Fun-ASR"), ("zh-CN", "千问 Fun-ASR")]),
        blurb: Text::localized([
            (
                "en",
                "Alibaba Qwen Fun-ASR realtime streaming: text appears as you speak; 8k models are downsampled automatically",
            ),
            (
                "zh-CN",
                "阿里千问 Fun-ASR 实时流式，边录边出字；8k 模型自动降采样",
            ),
        ]),
        docs: Some(
            "https://www.alibabacloud.com/help/en/model-studio/fun-asr-realtime-websocket-api"
                .to_owned(),
        ),
        streaming: true,
        fields: vec![
            Field {
                key: "api_key".to_owned(),
                label: Text::localized([("en", "API Key"), ("zh-CN", "API Key")]),
                help: Text::localized([
                    ("en", "Required. Create one in the DashScope console (sk-…)"),
                    ("zh-CN", "必填，DashScope 控制台创建（sk-…）"),
                ]),
                kind: FieldKind::Secret,
                required: true,
                advanced: false,
            },
            Field {
                key: "model".to_owned(),
                label: Text::localized([("en", "Model"), ("zh-CN", "模型")]),
                help: Text::localized([
                    (
                        "en",
                        "Default fun-asr-realtime; 8k models (name contains 8k) are downsampled to 8000 Hz",
                    ),
                    (
                        "zh-CN",
                        "默认 fun-asr-realtime；8k 型号（名字含 8k）自动降到 8000 Hz",
                    ),
                ]),
                kind: FieldKind::Text,
                required: false,
                advanced: true,
            },
            Field {
                key: "sample_rate".to_owned(),
                label: Text::localized([("en", "Sample rate"), ("zh-CN", "采样率")]),
                help: Text::localized([
                    ("en", "16000 for most models; 8000 for 8k models only"),
                    ("zh-CN", "16000 通用；8000 仅 8k 模型"),
                ]),
                kind: FieldKind::Choice {
                    options: vec!["16000".to_owned(), "8000".to_owned()],
                },
                required: false,
                advanced: true,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_matches_the_settings_page() {
        let engine = engine();
        let m = engine.manifest();
        assert_eq!(m.id, "dashscope");
        assert!(m.streaming);
        assert_eq!(m.title.get("zh-CN"), "千问 Fun-ASR");
        assert_eq!(m.title.get("en"), "Qwen Fun-ASR");
        assert_eq!(
            m.blurb.get("zh-CN"),
            "阿里千问 Fun-ASR 实时流式，边录边出字；8k 模型自动降采样"
        );
        assert_eq!(
            m.docs.as_deref(),
            Some(
                "https://www.alibabacloud.com/help/en/model-studio/fun-asr-realtime-websocket-api"
            )
        );
        let fields: Vec<_> = m
            .fields
            .iter()
            .map(|f| (f.key.as_str(), f.label.get("zh-CN"), f.required, f.advanced))
            .collect();
        assert_eq!(
            fields,
            [
                ("api_key", "API Key", true, false),
                ("model", "模型", false, true),
                ("sample_rate", "采样率", false, true),
            ]
        );
        assert_eq!(m.fields[0].kind, FieldKind::Secret);
        assert_eq!(
            m.fields[0].help.get("zh-CN"),
            "必填，DashScope 控制台创建（sk-…）"
        );
        assert_eq!(
            m.fields[1].help.get("zh-CN"),
            "默认 fun-asr-realtime；8k 型号（名字含 8k）自动降到 8000 Hz"
        );
        assert_eq!(
            m.fields[2].kind,
            FieldKind::Choice {
                options: vec!["16000".into(), "8000".into()]
            }
        );
        assert_eq!(m.fields[2].help.get("zh-CN"), "16000 通用；8000 仅 8k 模型");
        for f in &m.fields {
            assert!(!f.label.get("en").is_empty() && !f.help.get("en").is_empty());
        }
    }
}
