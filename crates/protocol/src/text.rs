use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Text an engine or a plugin shows to the user: one string for every
/// locale, or one per BCP-47 tag (`{"en": "Model", "zh-CN": "模型"}`).
/// Anya's own UI text lives in its catalog; this is for text that arrives
/// with a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Text {
    /// The same in every locale.
    Plain(String),
    /// Keyed by BCP-47 tag.
    Localized(BTreeMap<String, String>),
}

impl Default for Text {
    fn default() -> Text {
        Text::Plain(String::new())
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Text {
        Text::Plain(s.to_owned())
    }
}

impl Text {
    /// One string for each `(tag, text)`.
    pub fn localized<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Text {
        Text::Localized(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        )
    }

    /// The text for `locale`: its exact tag, then its language (`zh-TW`
    /// takes `zh` or `zh-CN`), then English, then whatever there is.
    pub fn get(&self, locale: &str) -> &str {
        let map = match self {
            Text::Plain(s) => return s,
            Text::Localized(map) => map,
        };
        let lang = |tag: &str| {
            tag.split(['-', '_'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        };
        let wanted = lang(locale);
        map.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(locale))
            .or_else(|| map.iter().find(|(k, _)| lang(k) == wanted))
            .or_else(|| map.iter().find(|(k, _)| lang(k) == "en"))
            .or_else(|| map.iter().next())
            .map_or("", |(_, v)| v.as_str())
    }

    /// No text in any locale.
    pub fn is_empty(&self) -> bool {
        match self {
            Text::Plain(s) => s.is_empty(),
            Text::Localized(map) => map.values().all(String::is_empty),
        }
    }
}
