//! The JSON lines between Anya and the plugin host, and the registry index.
//! Changing an expectation here is a protocol change, like a frame's.

use std::collections::BTreeMap;

use anya_protocol::{
    ErrorCode, HookCall, HostCommand, HostEvent, INDEX_VERSION, Index, IndexPlugin, IndexVersion,
    Limits, LogLevel, Manifest, PluginPackage, Provide, Recognition, Source, Text, Utterance,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[track_caller]
fn pinned<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T, wire: Value) {
    assert_eq!(serde_json::to_value(&value).expect("encode"), wire);
    let read: T = serde_json::from_value(wire).expect("decode");
    assert_eq!(read, value);
}

#[test]
fn host_events() {
    pinned(
        HostEvent::Ready {
            host_version: "1".to_owned(),
            protocol: 1,
        },
        json!({"type": "ready", "host_version": "1", "protocol": 1}),
    );
    pinned(
        HostEvent::Loaded {
            plugin: "echo".to_owned(),
            engine: Some(Manifest {
                id: "echo".to_owned(),
                title: "Echo".into(),
                blurb: Text::default(),
                docs: None,
                streaming: true,
                fields: Vec::new(),
            }),
            provides: vec![Provide::Engine, Provide::Action],
        },
        json!({"type": "loaded", "plugin": "echo",
               "engine": {"id": "echo", "title": "Echo", "streaming": true, "fields": []},
               "provides": ["engine", "action"]}),
    );
    pinned(
        HostEvent::Loaded {
            plugin: "polish".to_owned(),
            engine: None,
            provides: vec![Provide::Transform],
        },
        json!({"type": "loaded", "plugin": "polish", "provides": ["transform"]}),
    );
    pinned(
        HostEvent::LoadFailed {
            plugin: "echo".to_owned(),
            detail: "timed out".to_owned(),
        },
        json!({"type": "load_failed", "plugin": "echo", "detail": "timed out"}),
    );
    pinned(
        HostEvent::SessionReady {
            session: "s1".to_owned(),
        },
        json!({"type": "session_ready", "session": "s1"}),
    );
    pinned(
        HostEvent::Recognition {
            session: "s1".to_owned(),
            recognition: Recognition {
                text: "hi".to_owned(),
                utterances: vec![Utterance {
                    text: "hi".to_owned(),
                    definite: true,
                }],
                is_final: true,
            },
        },
        json!({"type": "recognition", "session": "s1", "text": "hi",
               "utterances": [{"text": "hi", "definite": true}], "final": true}),
    );
    pinned(
        HostEvent::SessionError {
            session: "s1".to_owned(),
            code: ErrorCode::NotConfigured,
            detail: "api_key".to_owned(),
        },
        json!({"type": "session_error", "session": "s1", "code": "not_configured", "detail": "api_key"}),
    );
    pinned(
        HostEvent::SessionClosed {
            session: "s1".to_owned(),
        },
        json!({"type": "session_closed", "session": "s1"}),
    );
    pinned(
        HostEvent::Log {
            plugin: "echo".to_owned(),
            level: LogLevel::Warn,
            message: "slow".to_owned(),
        },
        json!({"type": "log", "plugin": "echo", "level": "warn", "message": "slow"}),
    );
    pinned(
        HostEvent::Pong { seq: 7 },
        json!({"type": "pong", "seq": 7}),
    );
    pinned(
        HostEvent::ContextDone {
            call: "c1".to_owned(),
            hotwords: vec!["rquickjs".to_owned()],
            language: Some("en".to_owned()),
        },
        json!({"type": "context_done", "call": "c1", "hotwords": ["rquickjs"], "language": "en"}),
    );
    pinned(
        HostEvent::ContextDone {
            call: "c1".to_owned(),
            hotwords: Vec::new(),
            language: None,
        },
        json!({"type": "context_done", "call": "c1"}),
    );
    pinned(
        HostEvent::Transformed {
            call: "c2".to_owned(),
            text: "Hello.".to_owned(),
        },
        json!({"type": "transformed", "call": "c2", "text": "Hello."}),
    );
    pinned(
        HostEvent::ActionDone {
            call: "c3".to_owned(),
        },
        json!({"type": "action_done", "call": "c3"}),
    );
    pinned(
        HostEvent::CallFailed {
            call: "c3".to_owned(),
            detail: "vault not found".to_owned(),
        },
        json!({"type": "call_failed", "call": "c3", "detail": "vault not found"}),
    );
}

#[test]
fn host_commands() {
    pinned(
        HostCommand::Load {
            plugin: "echo".to_owned(),
            path: "/plugins/echo/1.0.0".to_owned(),
        },
        json!({"type": "load", "plugin": "echo", "path": "/plugins/echo/1.0.0"}),
    );
    pinned(
        HostCommand::Open {
            session: "s1".to_owned(),
            plugin: "echo".to_owned(),
            engine: "echo".to_owned(),
            language: Some("zh-CN".to_owned()),
            hotwords: vec!["Anya".to_owned()],
            settings: BTreeMap::from([("api_key".to_owned(), "k".to_owned())]),
        },
        json!({"type": "open", "session": "s1", "plugin": "echo", "engine": "echo",
               "language": "zh-CN", "hotwords": ["Anya"], "settings": {"api_key": "k"}}),
    );
    pinned(
        HostCommand::Open {
            session: "s1".to_owned(),
            plugin: "echo".to_owned(),
            engine: "echo".to_owned(),
            language: None,
            hotwords: Vec::new(),
            settings: BTreeMap::new(),
        },
        json!({"type": "open", "session": "s1", "plugin": "echo", "engine": "echo", "settings": {}}),
    );
    pinned(
        HostCommand::Audio {
            session: "s1".to_owned(),
            pcm: "AAEC".to_owned(),
        },
        json!({"type": "audio", "session": "s1", "pcm": "AAEC"}),
    );
    pinned(
        HostCommand::Finish {
            session: "s1".to_owned(),
        },
        json!({"type": "finish", "session": "s1"}),
    );
    pinned(
        HostCommand::Close {
            session: "s1".to_owned(),
        },
        json!({"type": "close", "session": "s1"}),
    );
    pinned(HostCommand::Shutdown, json!({"type": "shutdown"}));
    pinned(
        HostCommand::Ping { seq: 7 },
        json!({"type": "ping", "seq": 7}),
    );
    let about = HookCall {
        session: Some("s1".to_owned()),
        source: Source::Dictation,
        app: Some("com.apple.Notes".to_owned()),
        settings: BTreeMap::from([("vault".to_owned(), "~/notes".to_owned())]),
    };
    pinned(
        HostCommand::Context {
            call: "c1".to_owned(),
            plugin: "code".to_owned(),
            about: HookCall {
                session: None,
                ..about.clone()
            },
            language: Some("zh-CN".to_owned()),
            hotwords: vec!["Anya".to_owned()],
        },
        json!({"type": "context", "call": "c1", "plugin": "code", "source": "dictation",
               "app": "com.apple.Notes", "settings": {"vault": "~/notes"},
               "language": "zh-CN", "hotwords": ["Anya"]}),
    );
    pinned(
        HostCommand::Transform {
            call: "c2".to_owned(),
            plugin: "polish".to_owned(),
            text: "hello".to_owned(),
            about: about.clone(),
        },
        json!({"type": "transform", "call": "c2", "plugin": "polish", "text": "hello",
               "session": "s1", "source": "dictation", "app": "com.apple.Notes",
               "settings": {"vault": "~/notes"}}),
    );
    pinned(
        HostCommand::Action {
            call: "c3".to_owned(),
            plugin: "notes".to_owned(),
            text: "Hello.".to_owned(),
            about: HookCall {
                app: None,
                settings: BTreeMap::new(),
                ..about
            },
        },
        json!({"type": "action", "call": "c3", "plugin": "notes", "text": "Hello.",
               "session": "s1", "source": "dictation", "settings": {}}),
    );
    pinned(
        HostCommand::Cancel {
            call: "c2".to_owned(),
        },
        json!({"type": "cancel", "call": "c2"}),
    );
}

// A newer host's event or Anya's command reads as Unknown, so the side that
// is older skips it instead of failing.
#[test]
fn unknown_types_are_skipped() {
    let event: HostEvent = serde_json::from_str(r#"{"type":"metrics","cpu":1}"#).expect("decode");
    assert_eq!(event, HostEvent::Unknown);
    let command: HostCommand = serde_json::from_str(r#"{"type":"pause"}"#).expect("decode");
    assert_eq!(command, HostCommand::Unknown);
    let level: LogLevel = serde_json::from_str(r#""trace""#).expect("decode");
    assert_eq!(level, LogLevel::Unknown);
    let kind: Provide = serde_json::from_str(r#""trigger""#).expect("decode");
    assert_eq!(kind, Provide::Unknown);
}

#[test]
fn index() {
    let wire = json!({
        "version": 1,
        "plugins": [{
            "id": "echo",
            "name": {"en": "Echo", "zh-CN": "回声"},
            "description": "Counts bytes",
            "versions": [{
                "version": "1.2.0",
                "artifact": "https://example.org/echo-1.2.0.tgz",
                "integrity": "sha512-AAAA",
                "anya": ">=0.1.0",
                "protocol": 1,
                "provides": ["engine", "transform"],
                "host": true,
                "streaming": true,
                "permissions": ["network:api.example.org"]
            }]
        }]
    });
    pinned(
        Index {
            version: INDEX_VERSION,
            plugins: vec![IndexPlugin {
                id: "echo".to_owned(),
                name: Text::localized([("en", "Echo"), ("zh-CN", "回声")]),
                description: "Counts bytes".into(),
                versions: vec![IndexVersion {
                    version: "1.2.0".to_owned(),
                    artifact: "https://example.org/echo-1.2.0.tgz".to_owned(),
                    integrity: "sha512-AAAA".to_owned(),
                    anya: ">=0.1.0".to_owned(),
                    protocol: 1,
                    provides: vec![Provide::Engine, Provide::Transform],
                    host: true,
                    streaming: true,
                    permissions: vec!["network:api.example.org".to_owned()],
                }],
            }],
        },
        wire,
    );
}

// Fields a newer registry adds do not break an older Anya.
#[test]
fn index_ignores_new_fields() {
    let index: Index = serde_json::from_value(json!({
        "version": 1,
        "generated": "2026-10-30",
        "plugins": [{"id": "x", "name": "X", "homepage": "https://x", "versions": []}]
    }))
    .expect("decode");
    assert_eq!(index.plugins.len(), 1);
}

// The `anya` field of package.json, as plugin authors write it.
#[test]
fn plugin_package() {
    let package: PluginPackage = serde_json::from_value(json!({
        "id": "polish",
        "inline": "./inline.js",
        "host": "./host.js",
        "compat": ">=0.1.0",
        "protocol": 1,
        "permissions": ["network:api.openai.com"],
        "fields": [{"key": "api_key", "label": "API key", "kind": "secret", "required": true}],
        "limits": {"transform_ms": 3000}
    }))
    .expect("decode");
    assert_eq!(package.inline.as_deref(), Some("./inline.js"));
    assert_eq!(package.host.as_deref(), Some("./host.js"));
    assert_eq!(package.fields[0].key, "api_key");
    assert_eq!(
        package.limits,
        Limits {
            transform_ms: Some(3000),
            ..Limits::default()
        }
    );
    // A misspelt budget is an error, not a budget silently left at its default.
    let bad = serde_json::from_value::<PluginPackage>(json!({"limits": {"transfrom_ms": 1}}));
    assert!(bad.is_err());
}
