//! The settings form: which config values it shows, in what order, and how
//! each one reads, writes and parses.

use anya_config::{Config, DEFAULT_ADDRESS, EngineRole};
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
    DictationEngine,
    ContinuousEngine,
    RemoteEngine,
    EngineSettings,
    Language,
    Hotwords,
    HotwordsFile,
    ContinuousEnabled,
    ContinuousDevice,
    ContinuousHotkey,
    SilenceGate,
    Address,
    LanEnabled,
    LanPort,
    OverlayEnabled,
    Position,
    IdleVisible,
    ShowText,
    Scale,
    UpdateCheck,
    UpdateNow,
    DebugMode,
}

/// Every field, in the order the form lists them: tab by tab, card by card.
pub(crate) const FIELDS: &[Field] = &[
    Field::DictationEnabled,
    Field::Hotkeys,
    Field::Mode,
    Field::StopDelay,
    Field::DictationDevice,
    Field::Output,
    Field::DictationEngine,
    Field::ContinuousEngine,
    Field::RemoteEngine,
    Field::EngineSettings,
    Field::Language,
    Field::Hotwords,
    Field::HotwordsFile,
    Field::ContinuousEnabled,
    Field::ContinuousDevice,
    Field::ContinuousHotkey,
    Field::SilenceGate,
    Field::Address,
    Field::LanEnabled,
    Field::LanPort,
    Field::OverlayEnabled,
    Field::Position,
    Field::IdleVisible,
    Field::ShowText,
    Field::Scale,
    Field::UiLanguage,
    Field::Gain,
    Field::UpdateCheck,
    Field::UpdateNow,
    Field::DebugMode,
];

/// A run of fields shown together.
pub(crate) struct Card {
    pub(crate) fields: &'static [Field],
}

/// One page of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Dictation,
    Recognition,
    Continuous,
    Overlay,
    General,
}

impl Tab {
    /// In the order the tab bar lists them.
    pub(crate) const ALL: [Tab; 5] = [
        Tab::Dictation,
        Tab::Recognition,
        Tab::Continuous,
        Tab::Overlay,
        Tab::General,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Tab::Dictation => "tui.group.dictation",
            Tab::Recognition => "tui.group.recognition",
            Tab::Continuous => "tui.tab.continuous",
            Tab::Overlay => "tui.group.overlay",
            Tab::General => "tui.group.general",
        }
    }

    pub(crate) fn cards(self) -> &'static [Card] {
        use Field::*;
        match self {
            Tab::Dictation => &[
                Card {
                    fields: &[DictationEnabled, Hotkeys, Mode, StopDelay],
                },
                Card {
                    fields: &[DictationDevice, Output],
                },
            ],
            Tab::Recognition => &[
                Card {
                    fields: &[
                        DictationEngine,
                        ContinuousEngine,
                        RemoteEngine,
                        EngineSettings,
                    ],
                },
                Card {
                    fields: &[Language, Hotwords, HotwordsFile],
                },
            ],
            Tab::Continuous => &[
                Card {
                    fields: &[
                        ContinuousEnabled,
                        ContinuousDevice,
                        ContinuousHotkey,
                        SilenceGate,
                    ],
                },
                Card {
                    fields: &[Address, LanEnabled, LanPort],
                },
            ],
            Tab::Overlay => &[Card {
                fields: &[OverlayEnabled, Position, IdleVisible, ShowText, Scale],
            }],
            Tab::General => &[
                Card {
                    fields: &[UiLanguage, Gain],
                },
                Card {
                    fields: &[UpdateCheck, UpdateNow],
                },
                Card {
                    fields: &[DebugMode],
                },
            ],
        }
    }

    /// The tab that shows `field`.
    pub(crate) fn of(field: Field) -> Tab {
        Tab::ALL
            .into_iter()
            .find(|t| t.cards().iter().any(|c| c.fields.contains(&field)))
            .unwrap_or(Tab::Dictation)
    }

    /// The index in [`FIELDS`] of the tab's first field.
    pub(crate) fn first(self) -> usize {
        let field = self.cards()[0].fields[0];
        FIELDS.iter().position(|f| *f == field).unwrap_or(0)
    }

    /// The next tab along the bar, or the previous, wrapping.
    pub(crate) fn step(self, forward: bool) -> Tab {
        let at = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        let n = Tab::ALL.len();
        Tab::ALL[(at + if forward { 1 } else { n - 1 }) % n]
    }
}

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
            DictationEnabled | ContinuousEnabled | SilenceGate | LanEnabled | OverlayEnabled
            | IdleVisible | ShowText | UpdateCheck | DebugMode => Kind::Switch,
            UiLanguage | DictationDevice | Mode | Output | Language | ContinuousDevice
            | Position => Kind::Select,
            Gain | Hotkeys | StopDelay | Hotwords | HotwordsFile | ContinuousHotkey | Address
            | LanPort | Scale => Kind::Text,
            DictationEngine | ContinuousEngine | RemoteEngine | EngineSettings | UpdateNow => {
                Kind::Action
            }
        }
    }

    /// The use whose engine this field chooses.
    pub(crate) fn role(self) -> Option<EngineRole> {
        match self {
            Field::DictationEngine => Some(EngineRole::Dictation),
            Field::ContinuousEngine => Some(EngineRole::Continuous),
            Field::RemoteEngine => Some(EngineRole::Remote),
            _ => None,
        }
    }

    /// The field that chooses the engine for `role`.
    pub(crate) fn of_role(role: EngineRole) -> Field {
        match role {
            EngineRole::Dictation => Field::DictationEngine,
            EngineRole::Continuous => Field::ContinuousEngine,
            EngineRole::Remote => Field::RemoteEngine,
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
            DictationEngine => ("tui.field.engine_dictation", "tui.help.engine_dictation"),
            ContinuousEngine => ("tui.field.engine_continuous", "tui.help.engine_continuous"),
            RemoteEngine => ("tui.field.engine_remote", "tui.help.engine_remote"),
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
            LanEnabled => ("tui.field.lan_enabled", "tui.help.lan_enabled"),
            LanPort => ("tui.field.lan_port", "tui.help.lan_port"),
            OverlayEnabled => ("tui.field.overlay_enabled", "tui.help.overlay_enabled"),
            Position => ("tui.field.position", "tui.help.position"),
            IdleVisible => ("tui.field.idle_visible", "tui.help.idle_visible"),
            ShowText => ("tui.field.show_text", "tui.help.show_text"),
            Scale => ("tui.field.scale", "tui.help.scale"),
            UpdateCheck => ("tui.field.update_check", "tui.help.update_check"),
            UpdateNow => ("tui.field.update_now", "tui.help.update_now"),
            DebugMode => ("tui.field.debug_mode", "tui.help.debug_mode"),
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
            DictationEngine | ContinuousEngine | RemoteEngine => c
                .recognition
                .engine_id(self.role().unwrap_or(EngineRole::Dictation))
                .to_owned(),
            EngineSettings => String::new(),
            Language => c.recognition.language.clone(),
            Hotwords => c.recognition.hotwords.join(", "),
            HotwordsFile => c.recognition.hotwords_file.clone(),
            ContinuousEnabled => c.continuous.enabled.to_string(),
            ContinuousDevice => c.continuous.device.clone(),
            ContinuousHotkey => c.continuous.hotkey.clone(),
            SilenceGate => c.continuous.silence_gate.to_string(),
            Address => c.subscription.address.clone(),
            LanEnabled => c.subscription.lan.to_string(),
            LanPort => c.subscription.lan_port.to_string(),
            OverlayEnabled => c.overlay.enabled.to_string(),
            Position => c.overlay.position.clone(),
            IdleVisible => c.overlay.idle_visible.to_string(),
            ShowText => c.overlay.show_text.to_string(),
            Scale => c.overlay.scale.to_string(),
            UpdateCheck => c.updates.check.to_string(),
            UpdateNow => String::new(),
            DebugMode => c.debug.enabled.to_string(),
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
            DictationEngine | ContinuousEngine | RemoteEngine => {
                if let Some(role) = self.role() {
                    c.recognition.set_engine(role, value);
                }
            }
            EngineSettings => {}
            Language => c.recognition.language = value.to_owned(),
            ContinuousEnabled => c.continuous.enabled = on,
            ContinuousDevice => c.continuous.device = value.to_owned(),
            ContinuousHotkey => c.continuous.hotkey = value.to_owned(),
            SilenceGate => c.continuous.silence_gate = on,
            LanEnabled => c.subscription.lan = on,
            OverlayEnabled => c.overlay.enabled = on,
            Position => c.overlay.position = value.to_owned(),
            IdleVisible => c.overlay.idle_visible = on,
            ShowText => c.overlay.show_text = on,
            UpdateCheck => c.updates.check = on,
            UpdateNow => {}
            DebugMode => c.debug.enabled = on,
            Gain | StopDelay | Hotwords | HotwordsFile | Address | LanPort | Scale => {
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
            Field::LanPort => match text.parse::<u16>() {
                Ok(port) if port != 0 => c.subscription.lan_port = port,
                _ => return Err(Msg::new("tui.error.lan_port")),
            },
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
            DictationEngine => to
                .recognition
                .dictation
                .clone_from(&from.recognition.dictation),
            ContinuousEngine => to
                .recognition
                .continuous
                .clone_from(&from.recognition.continuous),
            RemoteEngine => to.recognition.remote.clone_from(&from.recognition.remote),
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
            LanEnabled => to.subscription.lan = from.subscription.lan,
            LanPort => to.subscription.lan_port = from.subscription.lan_port,
            OverlayEnabled => to.overlay.enabled = from.overlay.enabled,
            Position => to.overlay.position.clone_from(&from.overlay.position),
            IdleVisible => to.overlay.idle_visible = from.overlay.idle_visible,
            ShowText => to.overlay.show_text = from.overlay.show_text,
            Scale => to.overlay.scale = from.overlay.scale,
            UpdateCheck => to.updates.check = from.updates.check,
            UpdateNow => {}
            DebugMode => to.debug.enabled = from.debug.enabled,
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
    fn the_tabs_show_every_field_once_in_the_order_j_walks_them() {
        let shown: Vec<Field> = Tab::ALL
            .into_iter()
            .flat_map(|t| t.cards().iter().flat_map(|c| c.fields.iter().copied()))
            .collect();
        assert_eq!(shown, FIELDS);
        for (i, tab) in Tab::ALL.into_iter().enumerate() {
            assert_eq!(Tab::of(FIELDS[tab.first()]), tab);
            assert_eq!(Tab::ALL[(i + 1) % Tab::ALL.len()], tab.step(true));
        }
    }

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
        assert!(Field::LanPort.parse(&mut c, "0").is_err());
        assert!(Field::LanPort.parse(&mut c, "70000").is_err());
        assert_eq!(c.subscription.lan_port, 7591);
        assert!(Field::LanPort.parse(&mut c, " 9000 ").is_ok());
        assert_eq!(c.subscription.lan_port, 9000);
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
