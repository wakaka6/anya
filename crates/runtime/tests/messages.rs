//! Every refused value reads as a sentence in every locale, with its values in it.

use anya_i18n::{Catalog, LOCALES};
use anya_runtime::Invalid;

#[test]
fn every_invalid_value_has_a_message_with_its_values() {
    let all = [
        Invalid::HotkeyUnparsable { value: "V1".into() },
        Invalid::HotkeyTextKey { combo: "C1".into() },
        Invalid::HotkeyFnFunctionKey { combo: "C2".into() },
        Invalid::HotkeyTaken { combo: "C3".into() },
        Invalid::UnknownPosition { value: "V2".into() },
        Invalid::BadScale,
        Invalid::UnknownEngine { id: "E1".into() },
        Invalid::BadAddress {
            address: "A1".into(),
            detail: "D1".into(),
        },
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
