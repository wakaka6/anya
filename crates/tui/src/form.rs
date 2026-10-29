//! The settings form: which config values it shows, in what order, and how
//! each one reads, writes and parses.

use anya_config::{Config, DEFAULT_ADDRESS};
use anya_i18n::Catalog;
use anya_runtime::Invalid;

/// Text to show later, kept as a key so it follows a language change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Msg {
    key: &'static str,
    args: Vec<(&'static str, String)>,
}

impl Msg {
    pub(crate) fn new(key: &'static str) -> Msg {
        Msg {
            key,
            args: Vec::new(),
        }
    }

    pub(crate) fn arg(mut self, name: &'static str, value: impl Into<String>) -> Msg {
        self.args.push((name, value.into()));
        self
    }

    pub(crate) fn invalid(invalid: &Invalid) -> Msg {
        let (key, args) = invalid.message();
        Msg { key, args }
    }

    pub(crate) fn render(&self, catalog: &Catalog) -> String {
        let args: Vec<(&str, &str)> = self.args.iter().map(|(n, v)| (*n, v.as_str())).collect();
        catalog.tf(self.key, &args)
    }
}

/// One editable value of the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Field {
    UiLanguage,
    Gain,
    DictationEnabled,
    DictationDevice,
    Hotkeys,
    Mode,
    StopDelay,
    Output,
    Engine,
    EngineSettings,
    Language,
    Hotwords,
    HotwordsFile,
    ContinuousEnabled,
    ContinuousDevice,
    ContinuousHotkey,
    SilenceGate,
    Address,
    OverlayEnabled,
    Position,
    IdleVisible,
    ShowText,
    Scale,
}

/// Every field, in the order the form lists them.
pub(crate) const FIELDS: &[Field] = &[
    Field::UiLanguage,
    Field::Gain,
    Field::DictationEnabled,
    Field::DictationDevice,
    Field::Hotkeys,
    Field::Mode,
    Field::StopDelay,
    Field::Output,
    Field::Engine,
    Field::EngineSettings,
    Field::Language,
    Field::Hotwords,
    Field::HotwordsFile,
    Field::ContinuousEnabled,
    Field::ContinuousDevice,
    Field::ContinuousHotkey,
    Field::SilenceGate,
    Field::Address,
    Field::OverlayEnabled,
    Field::Position,
    Field::IdleVisible,
    Field::ShowText,
    Field::Scale,
];

/// How a field is edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// On or off; Space flips it.
    Switch,
    /// One of a list; j/k cycle while editing.
    Select,
    /// Typed text.
    Text,
    /// Enter opens a modal.
    Action,
}

impl Field {
    pub(crate) fn kind(self) -> Kind {
        use Field::*;
        match self {
            DictationEnabled | ContinuousEnabled | SilenceGate | OverlayEnabled | IdleVisible
            | ShowText => Kind::Switch,
            UiLanguage | DictationDevice | Mode | Output | Language | ContinuousDevice
            | Position => Kind::Select,
            Gain | Hotkeys | StopDelay | Hotwords | HotwordsFile | ContinuousHotkey | Address
            | Scale => Kind::Text,
            Engine | EngineSettings => Kind::Action,
        }
    }

    /// The group header shown above this field when it opens a group.
    pub(crate) fn group(self) -> Option<&'static str> {
        match self {
            Field::UiLanguage => Some("tui.group.general"),
            Field::DictationEnabled => Some("tui.group.dictation"),
            Field::Engine => Some("tui.group.recognition"),
            Field::ContinuousEnabled => Some("tui.group.continuous"),
            Field::OverlayEnabled => Some("tui.group.overlay"),
            _ => None,
        }
    }

    /// The label and help keys.
    pub(crate) fn text(self) -> (&'static str, &'static str) {
        use Field::*;
        match self {
            UiLanguage => ("tui.field.ui_language", "tui.help.ui_language"),
            Gain => ("tui.field.gain", "tui.help.gain"),
            DictationEnabled => ("tui.field.dictation_enabled", "tui.help.dictation_enabled"),
            DictationDevice => ("tui.field.dictation_device", "tui.help.dictation_device"),
            Hotkeys => ("tui.field.hotkeys", "tui.help.hotkeys"),
            Mode => ("tui.field.mode", "tui.help.mode"),
            StopDelay => ("tui.field.stop_delay", "tui.help.stop_delay"),
            Output => ("tui.field.output", "tui.help.output"),
            Engine => ("tui.field.engine", "tui.help.engine"),
            EngineSettings => ("tui.field.engine_settings", "tui.help.engine_settings"),
            Language => ("tui.field.language", "tui.help.language"),
            Hotwords => ("tui.field.hotwords", "tui.help.hotwords"),
            HotwordsFile => ("tui.field.hotwords_file", "tui.help.hotwords_file"),
            ContinuousEnabled => (
                "tui.field.continuous_enabled",
                "tui.help.continuous_enabled",
            ),
            ContinuousDevice => ("tui.field.continuous_device", "tui.help.continuous_device"),
            ContinuousHotkey => ("tui.field.continuous_hotkey", "tui.help.continuous_hotkey"),
            SilenceGate => ("tui.field.silence_gate", "tui.help.silence_gate"),
            Address => ("tui.field.address", "tui.help.address"),
            OverlayEnabled => ("tui.field.overlay_enabled", "tui.help.overlay_enabled"),
            Position => ("tui.field.position", "tui.help.position"),
            IdleVisible => ("tui.field.idle_visible", "tui.help.idle_visible"),
            ShowText => ("tui.field.show_text", "tui.help.show_text"),
            Scale => ("tui.field.scale", "tui.help.scale"),
        }
    }

    /// The value as the form shows and edits it. Switches give `true`/`false`.
    pub(crate) fn get(self, c: &Config) -> String {
        use Field::*;
        match self {
            UiLanguage => c.ui.language.clone(),
            Gain => c.audio.gain.to_string(),
            DictationEnabled => c.dictation.enabled.to_string(),
            DictationDevice => c.dictation.device.clone(),
            Hotkeys => c.dictation.hotkeys.clone(),
            Mode => match c.dictation.mode {
                anya_config::Mode::Toggle => "toggle",
                anya_config::Mode::Hold => "hold",
            }
            .to_owned(),
            StopDelay => c.dictation.stop_delay_ms.to_string(),
            Output => match c.dictation.output {
                anya_config::Output::Paste => "paste",
                anya_config::Output::Clipboard => "clipboard",
                anya_config::Output::None => "none",
            }
            .to_owned(),
            Engine | EngineSettings => c.recognition.engine.clone(),
            Language => c.recognition.language.clone(),
            Hotwords => c.recognition.hotwords.join(", "),
            HotwordsFile => c.recognition.hotwords_file.clone(),
            ContinuousEnabled => c.continuous.enabled.to_string(),
            ContinuousDevice => c.continuous.device.clone(),
            ContinuousHotkey => c.continuous.hotkey.clone(),
            SilenceGate => c.continuous.silence_gate.to_string(),
            Address => c.subscription.address.clone(),
            OverlayEnabled => c.overlay.enabled.to_string(),
            Position => c.overlay.position.clone(),
            IdleVisible => c.overlay.idle_visible.to_string(),
            ShowText => c.overlay.show_text.to_string(),
            Scale => c.overlay.scale.to_string(),
        }
    }

    /// Sets the value from a select option or a switch (`true`/`false`).
    pub(crate) fn assign(self, c: &mut Config, value: &str) {
        use Field::*;
        let on = value == "true";
        match self {
            UiLanguage => c.ui.language = value.to_owned(),
            DictationEnabled => c.dictation.enabled = on,
            DictationDevice => c.dictation.device = value.to_owned(),
            Hotkeys => c.dictation.hotkeys = value.to_owned(),
            Mode => {
                c.dictation.mode = if value == "hold" {
                    anya_config::Mode::Hold
                } else {
                    anya_config::Mode::Toggle
                }
            }
            Output => {
                c.dictation.output = match value {
                    "clipboard" => anya_config::Output::Clipboard,
                    "none" => anya_config::Output::None,
                    _ => anya_config::Output::Paste,
                }
            }
            Engine | EngineSettings => c.recognition.engine = value.to_owned(),
            Language => c.recognition.language = value.to_owned(),
            ContinuousEnabled => c.continuous.enabled = on,
            ContinuousDevice => c.continuous.device = value.to_owned(),
            ContinuousHotkey => c.continuous.hotkey = value.to_owned(),
            SilenceGate => c.continuous.silence_gate = on,
            OverlayEnabled => c.overlay.enabled = on,
            Position => c.overlay.position = value.to_owned(),
            IdleVisible => c.overlay.idle_visible = on,
            ShowText => c.overlay.show_text = on,
            Gain | StopDelay | Hotwords | HotwordsFile | Address | Scale => {
                // Typed fields go through `parse`; a value that does not
                // parse leaves the config as it was.
                let _ = self.parse(c, value);
            }
        }
    }

    /// Sets a typed field from what the user typed, or says why it cannot be used.
    pub(crate) fn parse(self, c: &mut Config, raw: &str) -> Result<(), Msg> {
        let text = raw.trim();
        match self {
            Field::Gain => {
                c.audio.gain = text.parse().map_err(|_| Msg::new("tui.error.gain"))?;
            }
            Field::StopDelay => {
                c.dictation.stop_delay_ms =
                    text.parse().map_err(|_| Msg::new("tui.error.stop_delay"))?;
            }
            Field::Scale => match text.parse::<f64>() {
                Ok(s) if s.is_finite() && s > 0.0 => c.overlay.scale = s,
                _ => return Err(Msg::invalid(&Invalid::BadScale)),
            },
            Field::Hotwords => c.recognition.hotwords = split_words(text),
            Field::HotwordsFile => c.recognition.hotwords_file = text.to_owned(),
            Field::Address => {
                c.subscription.address = if text.is_empty() {
                    DEFAULT_ADDRESS
                } else {
                    text
                }
                .to_owned();
            }
            other => other.assign(c, text),
        }
        Ok(())
    }

    /// Copies this field's value from `from` into `to`, exactly.
    pub(crate) fn copy(self, from: &Config, to: &mut Config) {
        use Field::*;
        match self {
            UiLanguage => to.ui.language.clone_from(&from.ui.language),
            Gain => to.audio.gain = from.audio.gain,
            DictationEnabled => to.dictation.enabled = from.dictation.enabled,
            DictationDevice => to.dictation.device.clone_from(&from.dictation.device),
            Hotkeys => to.dictation.hotkeys.clone_from(&from.dictation.hotkeys),
            Mode => to.dictation.mode = from.dictation.mode,
            StopDelay => to.dictation.stop_delay_ms = from.dictation.stop_delay_ms,
            Output => to.dictation.output = from.dictation.output,
            Engine => to.recognition.engine.clone_from(&from.recognition.engine),
            EngineSettings => to.recognition.engines.clone_from(&from.recognition.engines),
            Language => to
                .recognition
                .language
                .clone_from(&from.recognition.language),
            Hotwords => to
                .recognition
                .hotwords
                .clone_from(&from.recognition.hotwords),
            HotwordsFile => to
                .recognition
                .hotwords_file
                .clone_from(&from.recognition.hotwords_file),
            ContinuousEnabled => to.continuous.enabled = from.continuous.enabled,
            ContinuousDevice => to.continuous.device.clone_from(&from.continuous.device),
            ContinuousHotkey => to.continuous.hotkey.clone_from(&from.continuous.hotkey),
            SilenceGate => to.continuous.silence_gate = from.continuous.silence_gate,
            Address => to
                .subscription
                .address
                .clone_from(&from.subscription.address),
            OverlayEnabled => to.overlay.enabled = from.overlay.enabled,
            Position => to.overlay.position.clone_from(&from.overlay.position),
            IdleVisible => to.overlay.idle_visible = from.overlay.idle_visible,
            ShowText => to.overlay.show_text = from.overlay.show_text,
            Scale => to.overlay.scale = from.overlay.scale,
        }
    }
}

/// Words typed as a list: split on commas (ASCII and full-width), `、` and
/// newlines, trimmed, blanks and repeats dropped.
pub(crate) fn split_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split([',', '，', '、', '\n']).map(str::trim) {
        if !word.is_empty() && !out.iter().any(|w| w == word) {
            out.push(word.to_owned());
        }
    }
    out
}

/// A hotkey list split into combos: a comma ends a combo unless it follows
/// `+`, where it is the comma key (`Fn+,`).
pub(crate) fn split_hotkeys(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut part = String::new();
    for c in list.chars() {
        if (c == ',' || c == '，') && !part.trim_end().ends_with('+') {
            out.push(std::mem::take(&mut part));
        } else {
            part.push(c);
        }
    }
    out.push(part);
    out.into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The select label key of a dictation mode value.
pub(crate) fn mode_key(value: &str) -> Option<&'static str> {
    match value {
        "toggle" => Some("tui.mode.toggle"),
        "hold" => Some("tui.mode.hold"),
        _ => None,
    }
}

/// The select label key of an output value.
pub(crate) fn output_key(value: &str) -> Option<&'static str> {
    match value {
        "paste" => Some("tui.output.paste"),
        "clipboard" => Some("tui.output.clipboard"),
        "none" => Some("tui.output.none"),
        _ => None,
    }
}

/// The label key of an overlay position.
pub(crate) fn position_key(value: &str) -> Option<&'static str> {
    Some(match value {
        "notch" => "tui.position.notch",
        "top-left" => "tui.position.top_left",
        "top-center" => "tui.position.top_center",
        "top-right" => "tui.position.top_right",
        "bottom-left" => "tui.position.bottom_left",
        "bottom-center" => "tui.position.bottom_center",
        "bottom-right" => "tui.position.bottom_right",
        _ => return None,
    })
}

/// The label key of a recognition language tag.
pub(crate) fn language_key(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "" => "tui.language.auto",
        "zh-CN" => "tui.language.zh_cn",
        "en-US" => "tui.language.en_us",
        "ja-JP" => "tui.language.ja_jp",
        "ko-KR" => "tui.language.ko_kr",
        "fr-FR" => "tui.language.fr_fr",
        "de-DE" => "tui.language.de_de",
        "es-ES" => "tui.language.es_es",
        "ru-RU" => "tui.language.ru_ru",
        _ => return None,
    })
}

/// The label key of an interface language setting.
pub(crate) fn ui_language_key(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "system" => "tui.ui_language.system",
        "en" => "tui.ui_language.en",
        "zh-CN" => "tui.ui_language.zh_cn",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_split_like_the_user_typed_them() {
        assert_eq!(split_words("a, b，c、a\n d ,"), ["a", "b", "c", "d"]);
        assert_eq!(split_hotkeys("Fn, F13，Alt+,"), ["Fn", "F13", "Alt+,"]);
        assert_eq!(split_hotkeys(" "), Vec::<String>::new());
    }

    #[test]
    fn typed_numbers_are_checked() {
        let mut c = Config::default();
        assert!(Field::StopDelay.parse(&mut c, "-5").is_err());
        assert!(Field::StopDelay.parse(&mut c, "abc").is_err());
        assert_eq!(
            c.dictation.stop_delay_ms, 0,
            "a refused value changes nothing"
        );
        assert!(Field::StopDelay.parse(&mut c, " 300 ").is_ok());
        assert_eq!(c.dictation.stop_delay_ms, 300);
        assert!(Field::Scale.parse(&mut c, "0").is_err());
        assert!(Field::Scale.parse(&mut c, "NaN").is_err());
        assert!(Field::Scale.parse(&mut c, "1.5").is_ok());
        assert!(Field::Gain.parse(&mut c, "-6").is_ok());
        assert_eq!(c.audio.gain, -6);
        assert!(Field::Address.parse(&mut c, "  ").is_ok());
        assert_eq!(c.subscription.address, DEFAULT_ADDRESS);
    }

    #[test]
    fn every_field_copies_what_it_shows() {
        let from = {
            let mut c = Config::default();
            for f in FIELDS {
                if f.kind() == Kind::Switch {
                    let flipped = (f.get(&c) != "true").to_string();
                    f.assign(&mut c, &flipped);
                }
            }
            c.dictation.hotkeys = "F9".into();
            c.overlay.scale = 2.5;
            c
        };
        let mut to = Config::default();
        for f in FIELDS {
            f.copy(&from, &mut to);
            assert_eq!(f.get(&to), f.get(&from), "{f:?}");
        }
    }
}
