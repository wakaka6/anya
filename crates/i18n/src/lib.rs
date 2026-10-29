//! Every string Anya shows to the user, in every language it speaks.
//!
//! Text lives in `locales/<tag>/<area>.json`: flat objects from key to text,
//! keys prefixed by their area (`gui.save`, `overlay.copied`), `{name}`
//! marking a placeholder. Every locale has exactly the keys English has, with
//! the same placeholders; a test holds that. The settings page loads the same
//! text through [`Catalog::to_json`].
//!
//! Libraries below the app layer hold no user text: they return codes
//! ([`anya_protocol::ErrorCode`], typed errors) and the app looks them up here.

use std::collections::BTreeMap;
use std::process::Command;

use anya_protocol::ErrorCode;

/// The languages Anya ships, by BCP-47 tag. The first is the fallback.
pub const LOCALES: &[&str] = &["en", "zh-CN"];

/// One file per area, so each part of the app keeps its own text. In the
/// order of [`locale_files!`]; adding an area adds it to both.
#[cfg(test)]
const AREAS: &[&str] = &[
    "common", "error", "overlay", "tray", "gui", "tui", "doctor", "cli",
];

macro_rules! locale_files {
    ($tag:literal) => {
        [
            include_str!(concat!("../locales/", $tag, "/common.json")),
            include_str!(concat!("../locales/", $tag, "/error.json")),
            include_str!(concat!("../locales/", $tag, "/overlay.json")),
            include_str!(concat!("../locales/", $tag, "/tray.json")),
            include_str!(concat!("../locales/", $tag, "/gui.json")),
            include_str!(concat!("../locales/", $tag, "/tui.json")),
            include_str!(concat!("../locales/", $tag, "/doctor.json")),
            include_str!(concat!("../locales/", $tag, "/cli.json")),
        ]
    };
}

fn files(tag: &str) -> [&'static str; 8] {
    match tag {
        "zh-CN" => locale_files!("zh-CN"),
        _ => locale_files!("en"),
    }
}

/// A locale's text, every area merged. A file that is not a JSON object of
/// strings is a bug the tests catch; at run time it reads as empty.
fn load(tag: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for text in files(tag) {
        if let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(text) {
            out.extend(map);
        }
    }
    out
}

/// The text of one language, falling back to English for a key it lacks.
#[derive(Debug, Clone)]
pub struct Catalog {
    locale: &'static str,
    text: BTreeMap<String, String>,
    fallback: BTreeMap<String, String>,
}

impl Catalog {
    /// The catalog for `tag`, one of [`LOCALES`] or anything [`supported`]
    /// maps onto one.
    pub fn new(tag: &str) -> Catalog {
        let locale = supported(tag);
        Catalog {
            locale,
            text: load(locale),
            fallback: load(LOCALES[0]),
        }
    }

    /// The catalog for the user's setting: a tag, or `"system"` (or empty)
    /// for the system's language.
    pub fn for_setting(setting: &str) -> Catalog {
        match setting.trim() {
            "" | "system" => Catalog::new(&system_locale()),
            tag => Catalog::new(tag),
        }
    }

    /// The tag this catalog speaks, one of [`LOCALES`].
    pub fn locale(&self) -> &'static str {
        self.locale
    }

    /// The text for `key`; the key itself when no locale has it.
    pub fn t<'a>(&'a self, key: &'a str) -> &'a str {
        self.text
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }

    /// The text for `key` with each `{name}` replaced by its value.
    pub fn tf(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut out = self.t(key).to_owned();
        for (name, value) in args {
            out = out.replace(&format!("{{{name}}}"), value);
        }
        out
    }

    /// How to tell the user a session failed: the code in their language,
    /// with the untranslated detail after it when there is one.
    pub fn error(&self, code: ErrorCode, detail: Option<&str>) -> String {
        let key = match code {
            ErrorCode::NotConfigured => "error.not_configured",
            ErrorCode::Connect => "error.connect",
            ErrorCode::Upstream => "error.upstream",
            ErrorCode::Timeout => "error.timeout",
            ErrorCode::Audio => "error.audio",
            ErrorCode::Unknown => "error.unknown",
        };
        match detail.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => self.tf(
                "error.with_detail",
                &[("error", self.t(key)), ("detail", d)],
            ),
            None => self.t(key).to_owned(),
        }
    }

    /// Every key's text, fallbacks filled in, for the settings page.
    pub fn to_json(&self) -> String {
        let mut all = self.fallback.clone();
        all.extend(self.text.clone());
        serde_json::to_string(&all).unwrap_or_else(|_| "{}".to_owned())
    }
}

/// The shipped locale closest to `tag`: `zh`, `zh-Hans`, `zh_CN.UTF-8` and
/// the other Chinese tags give `zh-CN`; everything else gives English.
pub fn supported(tag: &str) -> &'static str {
    let tag = tag
        .split('.')
        .next()
        .unwrap_or("")
        .replace('_', "-")
        .to_ascii_lowercase();
    if let Some(exact) = LOCALES.iter().find(|l| l.to_ascii_lowercase() == tag) {
        return exact;
    }
    if tag == "zh" || tag.starts_with("zh-") {
        return "zh-CN";
    }
    LOCALES[0]
}

/// The system's language tag: the first of `LC_ALL`, `LC_MESSAGES`, `LANG`
/// and `LANGUAGE` that names one, then on macOS the first of the user's
/// preferred languages; `en` when none says.
pub fn system_locale() -> String {
    let env = ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| v.split(':').next().unwrap_or("").to_owned())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX" && !v.starts_with("C."));
    if let Some(tag) = env {
        return tag;
    }
    if cfg!(target_os = "macos")
        && let Ok(out) = Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
        && let Some(tag) = first_apple_language(&String::from_utf8_lossy(&out.stdout))
    {
        return tag;
    }
    LOCALES[0].to_owned()
}

/// The first entry `defaults read -g AppleLanguages` prints:
/// `(\n    "zh-Hans-CN",\n    "en-US"\n)`.
fn first_apple_language(out: &str) -> Option<String> {
    out.lines()
        .map(|l| l.trim().trim_end_matches(',').trim_matches('"'))
        .find(|l| !l.is_empty() && *l != "(" && *l != ")")
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<&str> {
        let mut out: Vec<&str> = text
            .split('{')
            .skip(1)
            .filter_map(|s| s.split_once('}').map(|(n, _)| n))
            .collect();
        out.sort_unstable();
        out
    }

    // A key missing from one language shows English mid-sentence; a
    // placeholder missing shows `{name}` to the user.
    #[test]
    fn every_locale_says_everything_english_says() {
        for tag in LOCALES {
            for (area, text) in AREAS.iter().zip(files(tag)) {
                let parsed = serde_json::from_str::<BTreeMap<String, String>>(text);
                let map = parsed.unwrap_or_else(|e| panic!("{tag}/{area}.json: {e}"));
                for key in map.keys() {
                    assert!(
                        key.starts_with(&format!("{area}.")),
                        "{tag}/{area}.json: {key}"
                    );
                }
            }
        }
        let en = load("en");
        for tag in &LOCALES[1..] {
            let other = load(tag);
            let missing: Vec<_> = en.keys().filter(|k| !other.contains_key(*k)).collect();
            let extra: Vec<_> = other.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{tag}: missing {missing:?}, extra {extra:?}"
            );
            for (key, text) in &en {
                assert_eq!(
                    placeholders(text),
                    placeholders(&other[key]),
                    "{tag}: {key}"
                );
            }
        }
    }

    #[test]
    fn system_tags_map_onto_shipped_locales() {
        for (tag, want) in [
            ("zh_CN.UTF-8", "zh-CN"),
            ("zh-Hans-CN", "zh-CN"),
            ("zh_TW", "zh-CN"),
            ("en_US.UTF-8", "en"),
            ("de-DE", "en"),
            ("", "en"),
        ] {
            assert_eq!(supported(tag), want, "{tag}");
        }
        let apple = "(\n    \"zh-Hans-CN\",\n    \"en-US\"\n)\n";
        assert_eq!(first_apple_language(apple).as_deref(), Some("zh-Hans-CN"));
    }

    #[test]
    fn an_error_reads_in_the_users_language_with_its_detail() {
        let zh = Catalog::new("zh-CN");
        let en = Catalog::new("en");
        assert_eq!(zh.error(ErrorCode::Timeout, None), zh.t("error.timeout"));
        assert_ne!(zh.t("error.timeout"), en.t("error.timeout"));
        let with = en.error(ErrorCode::Connect, Some("connection refused"));
        assert!(with.contains(en.t("error.connect")) && with.contains("connection refused"));
        assert_eq!(en.t("no.such.key"), "no.such.key");
    }
}
