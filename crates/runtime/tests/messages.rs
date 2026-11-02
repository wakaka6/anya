//! Every refused value reads as a sentence in every locale, with its values in it.

use anya_i18n::{Catalog, LOCALES};
use anya_runtime::{ClientIssue, Invalid, PairError, PluginIssue};

#[test]
fn every_invalid_value_has_a_message_with_its_values() {
    let all = [
        Invalid::HotkeyUnparsable { value: "V1".into() },
        Invalid::HotkeyTextKey { combo: "C1".into() },
        Invalid::HotkeyFnFunctionKey { combo: "C2".into() },
        Invalid::HotkeyTaken { combo: "C3".into() },
        Invalid::UnknownPosition { value: "V2".into() },
        Invalid::BadScale,
        Invalid::UnknownEngine {
            role: anya_config::EngineRole::Continuous,
            id: "E1".into(),
        },
        Invalid::BadAddress {
            address: "A1".into(),
            detail: "D1".into(),
        },
        Invalid::BadPluginId { id: "P1".into() },
    ];
    for tag in LOCALES {
        let catalog = Catalog::new(tag);
        for invalid in &all {
            let (key, args) = invalid.message();
            assert_ne!(catalog.t(key), key, "{tag}: {key} missing");
            let args: Vec<(&str, &str)> = args.iter().map(|(n, v)| (*n, v.as_str())).collect();
            let text = catalog.tf(key, &args);
            assert!(
                !text.contains('{'),
                "{tag}: {key} left a placeholder: {text}"
            );
            for (_, value) in &args {
                assert!(text.contains(value), "{tag}: {key} lost {value}: {text}");
            }
        }
    }
}

#[test]
fn every_plugin_issue_and_pairing_refusal_reads_in_every_locale_with_its_values() {
    let s = |v: &str| v.to_owned();
    let issues = [
        PluginIssue::Fetch {
            url: s("U1"),
            reason: s("R1"),
        },
        PluginIssue::Index { reason: s("R2") },
        PluginIssue::IndexVersion {
            found: 71,
            supported: 72,
        },
        PluginIssue::NotInIndex {
            id: s("I1"),
            version: s("V1"),
        },
        PluginIssue::Incompatible {
            id: s("I2"),
            version: s("V2"),
            reason: s("R3"),
        },
        PluginIssue::Integrity { what: s("W1") },
        PluginIssue::IntegrityFormat { value: s("X1") },
        PluginIssue::Invalid { value: s("X2") },
        PluginIssue::NotInstalled { id: s("I3") },
        PluginIssue::Archive {
            what: s("W2"),
            reason: s("R4"),
        },
        PluginIssue::Io {
            path: s("P1"),
            reason: s("R5"),
        },
        PluginIssue::Bun { reason: s("R6") },
        PluginIssue::BunMissing {
            version: s("1.2.0"),
        },
    ];
    let mut coded: Vec<_> = issues.iter().map(PluginIssue::message).collect();
    coded.push(PairError::LanOff.message());
    coded.push(ClientIssue::UnknownClient { id: s("C1") }.message());
    coded.push(ClientIssue::Write { detail: s("D2") }.message());
    coded.push(PairError::LanDown { detail: s("D1") }.message());
    for tag in LOCALES {
        let catalog = Catalog::new(tag);
        for (key, args) in &coded {
            assert_ne!(catalog.t(key), *key, "{tag}: {key} missing");
            let pairs: Vec<(&str, &str)> = args.iter().map(|(n, v)| (*n, v.as_str())).collect();
            let text = catalog.tf(key, &pairs);
            assert!(
                !text.contains('{'),
                "{tag}: {key} left a placeholder: {text}"
            );
            for (_, value) in &pairs {
                assert!(text.contains(value), "{tag}: {key} lost {value}: {text}");
            }
        }
    }
}
