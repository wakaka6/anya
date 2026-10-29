//! Doubao streaming recognition: Volcengine's large-model ASR over its
//! bidirectional WebSocket, with live partials, a second pass that settles
//! each sentence, and hotwords.
//!
//! Settings beyond the manifest's fields, for hand-edited configs:
//! `stream_mode` (`async`, the default, or `nostream`), `language`,
//! `enable_itn`, `enable_punc`, `enable_ddc`, `enable_nonstream` (booleans)
//! and the console's context tables `boosting_table_name`/`_id`,
//! `correct_table_name`/`_id`, `regex_correct_table_name`/`_id`.

use std::collections::BTreeMap;
use std::sync::Arc;

use anya_core::engine::{Engine, EngineError, Session, SessionParams};
use anya_protocol::{Field, FieldKind, Manifest, Text};

mod config;
mod frame;
mod session;
mod transcript;

/// The Doubao engine.
pub fn engine() -> Arc<dyn Engine> {
    Arc::new(Doubao {
        manifest: manifest(),
    })
}

struct Doubao {
    manifest: Manifest,
}

impl Engine for Doubao {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// An API key alone, or an app id with an access key; choices must be
    /// one of their options.
    fn check(&self, settings: &BTreeMap<String, String>) -> Result<(), EngineError> {
        config::Config::parse(settings, None).map(|_| ())
    }

    fn open(&self, params: SessionParams) -> Session {
        session::open(params)
    }
}

fn text(en: &str, zh: &str) -> Text {
    Text::localized([("en", en), ("zh-CN", zh)])
}

fn choice(options: &[&str]) -> FieldKind {
    FieldKind::Choice {
        options: options.iter().map(|&o| o.to_owned()).collect(),
    }
}

struct Spec {
    key: &'static str,
    label: (&'static str, &'static str),
    help: (&'static str, &'static str),
    kind: FieldKind,
    required: bool,
    advanced: bool,
}

fn manifest() -> Manifest {
    let fields = [
        Spec {
            key: "api_key",
            label: ("API Key", "API Key"),
            help: (
                "X-Api-Key from the new console; when set, the legacy credentials below are ignored",
                "新版控制台 X-Api-Key；填了就忽略下方旧版凭据",
            ),
            kind: FieldKind::Secret,
            required: false,
            advanced: false,
        },
        Spec {
            key: "app_id",
            label: ("App ID", "App ID"),
            help: (
                "App ID from the legacy console (X-Api-App-Key)",
                "旧版控制台 App ID（X-Api-App-Key）",
            ),
            kind: FieldKind::Text,
            required: false,
            advanced: true,
        },
        Spec {
            key: "access_key",
            label: ("Access Key", "Access Key"),
            help: (
                "Access Token from the legacy console (X-Api-Access-Key)",
                "旧版 Access Token（X-Api-Access-Key）",
            ),
            kind: FieldKind::Secret,
            required: true,
            advanced: true,
        },
        Spec {
            key: "model_version",
            label: ("Model version", "模型版本"),
            help: (
                "1.0 = bigasr (legacy resources), 2.0 = seedasr (new console)",
                "1.0 = bigasr（旧资源），2.0 = seedasr（新控制台）",
            ),
            kind: choice(&["1.0", "2.0"]),
            required: false,
            advanced: false,
        },
        Spec {
            key: "stream_billing",
            label: ("Streaming billing", "流式计费"),
            help: (
                "duration bills by audio length, concurrent by concurrency; match your plan",
                "duration 按时长 / concurrent 按并发，对照套餐选择",
            ),
            kind: choice(&["duration", "concurrent"]),
            required: false,
            advanced: true,
        },
        Spec {
            key: "resource_id",
            label: ("Resource ID", "Resource ID"),
            help: (
                "Derived from model version and billing when empty; set it only if the console gives you another resource ID",
                "留空则按模型版本+计费推导；除非控制台另有资源 ID 一般不填",
            ),
            kind: FieldKind::Text,
            required: false,
            advanced: true,
        },
        Spec {
            key: "enable_lid",
            label: ("Mixed Chinese and English", "中英混识别"),
            help: (
                "Turn on for mixed Chinese and English speech, or dialects",
                "中英混说/方言时开启",
            ),
            kind: choice(&["false", "true"]),
            required: false,
            advanced: true,
        },
        Spec {
            key: "base_host",
            label: ("Base Host", "Base Host"),
            help: (
                "Default openspeech.bytedance.com; change it only for a self-hosted proxy or gateway",
                "默认 openspeech.bytedance.com；仅自建代理/网关时改",
            ),
            kind: FieldKind::Text,
            required: false,
            advanced: true,
        },
    ];
    Manifest {
        id: "doubao".to_owned(),
        title: text("Doubao", "豆包语音"),
        blurb: text(
            "Doubao large-model bidirectional streaming ASR: text while you speak, a second pass to correct it, hotwords",
            "豆包大模型双向流式 ASR，边说边出字 + 二遍校正，支持热词",
        ),
        docs: Some("https://www.volcengine.com/docs/6561/1395867".to_owned()),
        streaming: true,
        fields: fields
            .into_iter()
            .map(|s| Field {
                key: s.key.to_owned(),
                label: text(s.label.0, s.label.1),
                help: text(s.help.0, s.help.1),
                kind: s.kind,
                required: s.required,
                advanced: s.advanced,
            })
            .collect(),
    }
}
