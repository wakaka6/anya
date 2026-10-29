//! The manifest an engine plugin sends when it loads.

use anya_protocol::{FieldKind, Manifest, Text};
use serde_json::json;

#[test]
fn a_plugin_manifest_reads_as_settings_fields() {
    let manifest: Manifest = serde_json::from_value(json!({
        "id": "whisper-local",
        "title": "Whisper (local)",
        "streaming": false,
        "fields": [
            {"key": "base_url", "label": "Base URL", "kind": "text", "advanced": true},
            {"key": "api_key", "label": "API Key", "kind": "secret", "required": true},
            {"key": "model", "label": "Model", "kind": "choice", "options": ["base", "small"]}
        ]
    }))
    .expect("decode");

    let kinds: Vec<&FieldKind> = manifest.fields.iter().map(|f| &f.kind).collect();
    assert_eq!(
        kinds,
        [
            &FieldKind::Text,
            &FieldKind::Secret,
            &FieldKind::Choice {
                options: vec!["base".to_owned(), "small".to_owned()]
            },
        ]
    );
    assert!(manifest.fields[0].advanced && !manifest.fields[0].required);
    assert!(manifest.fields[1].required);
    assert!(manifest.blurb.is_empty());
    assert_eq!(manifest.docs, None);

    let again: Manifest =
        serde_json::from_value(serde_json::to_value(&manifest).expect("encode")).expect("decode");
    assert_eq!(again, manifest);
}

// A plugin built against a newer Anya may use a kind this one cannot draw;
// loading it fails instead of showing a field the user cannot fill in.
#[test]
fn an_unknown_field_kind_is_refused() {
    let read = serde_json::from_value::<Manifest>(json!({
        "id": "x", "title": "X", "streaming": true,
        "fields": [{"key": "k", "label": "K", "kind": "slider"}]
    }));
    assert!(read.is_err());
}

// A plugin labels its settings in the languages it knows; every UI shows the
// closest one to its own and never an empty label.
#[test]
fn a_label_falls_back_to_the_closest_language() {
    let manifest: Manifest = serde_json::from_value(json!({
        "id": "x", "title": "X", "streaming": true,
        "fields": [{"key": "model", "kind": "text",
                    "label": {"en": "Model", "zh-CN": "模型", "ja": "モデル"}}]
    }))
    .expect("decode");
    let label = &manifest.fields[0].label;
    assert_eq!(label.get("zh-CN"), "模型");
    assert_eq!(label.get("zh-TW"), "模型");
    assert_eq!(label.get("ja-JP"), "モデル");
    assert_eq!(label.get("de"), "Model");
    assert_eq!(manifest.title.get("de"), "X");

    let only_french = Text::localized([("fr", "Modèle")]);
    assert_eq!(only_french.get("en"), "Modèle");
}
