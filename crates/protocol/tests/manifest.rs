//! The manifest an engine plugin sends when it loads.

use anya_protocol::{FieldKind, Manifest};
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
    assert_eq!(manifest.blurb, "");
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
