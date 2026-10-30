//! The JSON lines between Anya and the plugin host, and the registry index.
//! Changing an expectation here is a protocol change, like a frame's.

use std::collections::BTreeMap;

use anya_protocol::{
    ErrorCode, HostCommand, HostEvent, INDEX_VERSION, Index, IndexPlugin, IndexVersion, LogLevel,
    Manifest, PluginKind, Recognition, Text, Utterance,
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
            manifest: Manifest {
                id: "echo".to_owned(),
                title: "Echo".into(),
                blurb: Text::default(),
                docs: None,
                streaming: true,
                fields: Vec::new(),
            },
        },
        json!({"type": "loaded", "plugin": "echo",
               "manifest": {"id": "echo", "title": "Echo", "streaming": true, "fields": []}}),
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
    let kind: PluginKind = serde_json::from_str(r#""sink""#).expect("decode");
    assert_eq!(kind, PluginKind::Unknown);
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
                "kind": "engine",
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
                    kind: PluginKind::Engine,
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
