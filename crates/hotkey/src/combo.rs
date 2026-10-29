//! Hotkey combos: what a config names and what an event reports.

use std::fmt;
use std::ops::{BitOr, BitOrAssign};
use std::str::FromStr;

/// A set of modifiers. Left and right keys of a modifier are one modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Mods(u8);

impl Mods {
    /// No modifier.
    pub const NONE: Mods = Mods(0);
    /// Control.
    pub const CTRL: Mods = Mods(1);
    /// Alt, Option on a Mac keyboard.
    pub const ALT: Mods = Mods(1 << 1);
    /// Shift.
    pub const SHIFT: Mods = Mods(1 << 2);
    /// Super: Command on a Mac keyboard, the Windows key elsewhere.
    pub const SUPER: Mods = Mods(1 << 3);
    /// Fn, where the keyboard reports it.
    pub const FN: Mods = Mods(1 << 4);

    /// Each modifier with its canonical name, in canonical order.
    const NAMED: [(Mods, &'static str); 5] = [
        (Mods::CTRL, "Ctrl"),
        (Mods::ALT, "Alt"),
        (Mods::SHIFT, "Shift"),
        (Mods::SUPER, "Super"),
        (Mods::FN, "Fn"),
    ];

    /// Whether every modifier of `other` is in `self`.
    pub const fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no modifier is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `self` without the modifiers of `other`.
    pub const fn without(self, other: Mods) -> Mods {
        Mods(self.0 & !other.0)
    }

    fn from_name(part: &str) -> Option<Mods> {
        const ALIASES: [(&str, Mods); 11] = [
            ("ctrl", Mods::CTRL),
            ("control", Mods::CTRL),
            ("alt", Mods::ALT),
            ("option", Mods::ALT),
            ("shift", Mods::SHIFT),
            ("super", Mods::SUPER),
            ("cmd", Mods::SUPER),
            ("command", Mods::SUPER),
            ("win", Mods::SUPER),
            ("fn", Mods::FN),
            ("function", Mods::FN),
        ];
        ALIASES
            .iter()
            .find(|(name, _)| part.eq_ignore_ascii_case(name))
            .map(|&(_, m)| m)
    }
}

impl BitOr for Mods {
    type Output = Mods;

    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

impl BitOrAssign for Mods {
    fn bitor_assign(&mut self, rhs: Mods) {
        self.0 |= rhs.0;
    }
}

impl fmt::Display for Mods {
    /// `Ctrl+Alt+Shift+Super+Fn` order; empty for no modifier.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (m, name) in Mods::NAMED {
            if self.contains(m) {
                if !first {
                    f.write_str("+")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

macro_rules! keys {
    ($($variant:ident = $name:literal $(| $alias:literal)*,)*) => {
        /// A key a hotkey names besides its modifiers.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Key {
            /// No key: the combo is modifiers alone.
            None,
            $(
                #[doc = concat!("The `", $name, "` key.")]
                $variant,
            )*
        }

        impl Key {
            /// Every key a combo can name, [`Key::None`] excepted.
            pub const ALL: &[Key] = &[$(Key::$variant,)*];

            /// The canonical name config files store; empty for [`Key::None`].
            pub const fn name(self) -> &'static str {
                match self {
                    Key::None => "",
                    $(Key::$variant => $name,)*
                }
            }

            /// The key a combo part names, ignoring case.
            fn from_name(part: &str) -> Option<Key> {
                $(
                    if part.eq_ignore_ascii_case($name) $(|| part.eq_ignore_ascii_case($alias))* {
                        return Some(Key::$variant);
                    }
                )*
                None
            }
        }
    };
}

keys! {
    A = "A", B = "B", C = "C", D = "D", E = "E", F = "F", G = "G", H = "H", I = "I",
    J = "J", K = "K", L = "L", M = "M", N = "N", O = "O", P = "P", Q = "Q", R = "R",
    S = "S", T = "T", U = "U", V = "V", W = "W", X = "X", Y = "Y", Z = "Z",
    Digit0 = "0", Digit1 = "1", Digit2 = "2", Digit3 = "3", Digit4 = "4",
    Digit5 = "5", Digit6 = "6", Digit7 = "7", Digit8 = "8", Digit9 = "9",
    Num0 = "Num0", Num1 = "Num1", Num2 = "Num2", Num3 = "Num3", Num4 = "Num4",
    Num5 = "Num5", Num6 = "Num6", Num7 = "Num7", Num8 = "Num8", Num9 = "Num9",
    Space = "Space",
    Backtick = "`", Minus = "-", Equal = "=", LeftBracket = "[", RightBracket = "]",
    Backslash = "\\", Semicolon = ";", Quote = "'", Comma = ",", Period = ".", Slash = "/",
    F1 = "F1", F2 = "F2", F3 = "F3", F4 = "F4", F5 = "F5", F6 = "F6",
    F7 = "F7", F8 = "F8", F9 = "F9", F10 = "F10", F11 = "F11", F12 = "F12",
    F13 = "F13", F14 = "F14", F15 = "F15", F16 = "F16", F17 = "F17", F18 = "F18",
    F19 = "F19", F20 = "F20", F21 = "F21", F22 = "F22", F23 = "F23", F24 = "F24",
    Tab = "Tab", Enter = "Enter" | "return", Escape = "Escape" | "esc",
    Backspace = "Backspace", CapsLock = "CapsLock",
    Up = "Up", Down = "Down", Left = "Left", Right = "Right",
    Home = "Home", End = "End", PageUp = "PageUp", PageDown = "PageDown",
    Insert = "Insert", Delete = "Delete",
    Menu = "Menu" | "apps" | "application",
}

impl Key {
    /// Whether pressing the key types a character into a focused field:
    /// letters, digits, keypad digits, Space and punctuation.
    pub fn is_text(self) -> bool {
        (Key::A..=Key::Slash).contains(&self)
    }

    /// Whether the key is one of F1 to F24.
    pub fn is_function(self) -> bool {
        (Key::F1..=Key::F24).contains(&self)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A hotkey: modifiers and a key ([`Key::None`] for modifiers alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Combo {
    /// The modifiers held.
    pub mods: Mods,
    /// The key pressed, or [`Key::None`] for a modifier-only combo.
    pub key: Key,
}

impl Combo {
    /// A combo of `mods` and `key`.
    pub const fn new(mods: Mods, key: Key) -> Combo {
        Combo { mods, key }
    }

    /// A key pressed with no modifier, such as `F13`.
    pub const fn key(key: Key) -> Combo {
        Combo::new(Mods::NONE, key)
    }

    /// Whether the combo is modifiers alone, such as `Alt+Super` or `Fn`.
    pub const fn is_modifier_only(&self) -> bool {
        matches!(self.key, Key::None) && !self.mods.is_empty()
    }

    /// Whether the combo names nothing: no modifier and no key.
    pub const fn is_empty(&self) -> bool {
        matches!(self.key, Key::None) && self.mods.is_empty()
    }

    /// Whether the combo may serve as a global hotkey. A key that types a
    /// character is refused unless Fn is held with it (it would fire while
    /// typing), and Fn with F1 to F24 is refused (macOS treats those as
    /// system controls).
    pub fn check_global(&self) -> Result<(), Disallowed> {
        if self.is_empty() {
            return Err(Disallowed::Empty);
        }
        let with_fn = self.mods.contains(Mods::FN);
        if with_fn && self.key.is_function() {
            return Err(Disallowed::FnWithFunctionKey(*self));
        }
        if self.key.is_text() && !with_fn {
            return Err(Disallowed::TextKey(*self));
        }
        Ok(())
    }
}

impl fmt::Display for Combo {
    /// The canonical text: modifiers in `Ctrl+Alt+Shift+Super+Fn` order,
    /// then the key, joined with `+`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.mods)?;
        if self.key != Key::None {
            if !self.mods.is_empty() {
                f.write_str("+")?;
            }
            f.write_str(self.key.name())?;
        }
        Ok(())
    }
}

impl FromStr for Combo {
    type Err = ParseError;

    /// Parses `+`-separated names, ignoring case and spaces around them.
    fn from_str(s: &str) -> Result<Combo, ParseError> {
        let input = s.trim();
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        let mut combo = Combo::new(Mods::NONE, Key::None);
        for part in input.split('+').map(str::trim) {
            if let Some(m) = Mods::from_name(part) {
                combo.mods |= m;
                continue;
            }
            let Some(key) = Key::from_name(part) else {
                return Err(ParseError::UnknownKey {
                    part: part.to_owned(),
                    input: input.to_owned(),
                });
            };
            if combo.key != Key::None {
                return Err(ParseError::MultipleKeys(input.to_owned()));
            }
            combo.key = key;
        }
        Ok(combo)
    }
}

/// Text that names no combo.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The text is empty.
    #[error("empty hotkey string")]
    Empty,
    /// Two keys that are not modifiers, such as `A+B`.
    #[error("multiple keys in {0:?}")]
    MultipleKeys(String),
    /// A part names no key or modifier (an empty part included, as in `Ctrl+`).
    #[error("unknown key {part:?} in {input:?}")]
    UnknownKey {
        /// The part, as written.
        part: String,
        /// The whole combo, as written.
        input: String,
    },
}

/// Why a combo cannot be a global hotkey ([`Combo::check_global`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Disallowed {
    /// Fn with F1 to F24.
    #[error("Fn cannot be combined with F1-F24: {0}")]
    FnWithFunctionKey(Combo),
    /// A key that types a character, without Fn.
    #[error("a hotkey cannot be a plain character key: {0}")]
    TextKey(Combo),
    /// No modifier and no key.
    #[error("a hotkey needs a key or a modifier")]
    Empty,
}

impl Disallowed {
    /// A stable code for the app to translate.
    pub fn code(&self) -> &'static str {
        match self {
            Disallowed::FnWithFunctionKey(_) => "hotkey_fn_function_key",
            Disallowed::TextKey(_) => "hotkey_text_key",
            Disallowed::Empty => "hotkey_empty",
        }
    }
}

/// Parses a comma-separated list such as `Fn, F13`. A full-width `，`
/// separates too; a comma right after `+` is the comma key, so `Fn+,` is one
/// combo. Empty items are skipped and repeats dropped, first kept. An empty
/// list is valid: it registers nothing.
pub fn parse_list(s: &str) -> Result<Vec<Combo>, ParseError> {
    let s = s.replace('，', ",");
    let mut combos = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices().chain([(s.len(), ',')]) {
        if c != ',' || s[start..i].trim_end().ends_with('+') && i < s.len() {
            continue;
        }
        let part = s[start..i].trim();
        start = i + 1;
        if part.is_empty() {
            continue;
        }
        let combo: Combo = part.parse()?;
        if !combos.contains(&combo) {
            combos.push(combo);
        }
    }
    Ok(combos)
}

/// The canonical text of a list, joined with `, `: what [`parse_list`] reads.
pub fn format_list(combos: &[Combo]) -> String {
    combos
        .iter()
        .map(Combo::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Combo {
        match s.parse() {
            Ok(c) => c,
            Err(e) => panic!("{s:?}: {e}"),
        }
    }

    #[test]
    fn modifier_aliases_are_case_insensitive_and_print_canonically() {
        let cases = [
            ("ctrl+SHIFT+f1", "Ctrl+Shift+F1"),
            ("Control+Alt+Tab", "Ctrl+Alt+Tab"),
            ("option+command", "Alt+Super"),
            ("Super+Alt", "Alt+Super"),
            ("cmd+Option", "Alt+Super"),
            ("win+alt", "Alt+Super"),
            ("Function", "Fn"),
            ("fn", "Fn"),
            ("Ctrl", "Ctrl"),
            ("shift+ctrl+alt+super+fn", "Ctrl+Alt+Shift+Super+Fn"),
            (" Alt + F8 ", "Alt+F8"),
            ("F13", "F13"),
            ("f24", "F24"),
        ];
        for (input, canonical) in cases {
            assert_eq!(parse(input).to_string(), canonical, "{input}");
        }
    }

    #[test]
    fn every_key_name_and_alias_parses() {
        let aliases = [
            ("return", Key::Enter),
            ("esc", Key::Escape),
            ("apps", Key::Menu),
            ("Application", Key::Menu),
            ("pageup", Key::PageUp),
            ("NUM7", Key::Num7),
            ("capslock", Key::CapsLock),
            ("`", Key::Backtick),
            ("\\", Key::Backslash),
            ("'", Key::Quote),
        ];
        for (name, key) in aliases {
            assert_eq!(parse(name), Combo::key(key), "{name}");
        }
        for &key in Key::ALL {
            let combo = Combo::new(Mods::CTRL | Mods::ALT, key);
            assert_eq!(parse(&combo.to_string()), combo, "{key:?}");
            assert_eq!(parse(&key.name().to_lowercase()), Combo::key(key));
        }
    }

    #[test]
    fn combos_round_trip_through_their_text() {
        let modifier_only = Combo::new(Mods::ALT | Mods::SUPER, Key::None);
        assert!(modifier_only.is_modifier_only());
        assert_eq!(parse("Alt+Super"), modifier_only);
        assert_eq!(parse("Fn+F5"), Combo::new(Mods::FN, Key::F5));
        assert_eq!(parse("Fn+A"), Combo::new(Mods::FN, Key::A));
        assert_eq!(parse("Fn+,"), Combo::new(Mods::FN, Key::Comma));
        assert_eq!(parse("Menu"), Combo::key(Key::Menu));
        for text in [
            "Fn+,",
            "Ctrl+Alt+Space",
            "Alt+Super",
            "Fn",
            "Menu",
            "Shift+=",
        ] {
            assert_eq!(parse(text).to_string(), text);
        }
        assert_eq!(Combo::new(Mods::NONE, Key::None).to_string(), "");
    }

    #[test]
    fn malformed_combos_are_refused() {
        assert_eq!("".parse::<Combo>(), Err(ParseError::Empty));
        assert_eq!("   ".parse::<Combo>(), Err(ParseError::Empty));
        assert_eq!(
            "A+B".parse::<Combo>(),
            Err(ParseError::MultipleKeys("A+B".into()))
        );
        assert_eq!(
            "Ctrl+".parse::<Combo>(),
            Err(ParseError::UnknownKey {
                part: String::new(),
                input: "Ctrl+".into()
            })
        );
        // `+` is never a key.
        assert!(matches!(
            "Ctrl++".parse::<Combo>(),
            Err(ParseError::UnknownKey { .. })
        ));
        for unparseable in ["PrintScreen", "ScrollLock", "Pause", "NumLock", "Hyper"] {
            assert!(
                matches!(
                    unparseable.parse::<Combo>(),
                    Err(ParseError::UnknownKey { .. })
                ),
                "{unparseable}"
            );
        }
        assert_eq!(
            ParseError::UnknownKey {
                part: "x1".into(),
                input: "Ctrl+x1".into()
            }
            .to_string(),
            r#"unknown key "x1" in "Ctrl+x1""#
        );
    }

    #[test]
    fn lists_split_on_commas_except_the_comma_key() {
        let list = |s: &str| match parse_list(s) {
            Ok(l) => format_list(&l),
            Err(e) => panic!("{s:?}: {e}"),
        };
        assert_eq!(list("Fn, F13"), "Fn, F13");
        assert_eq!(list("Fn，Menu"), "Fn, Menu");
        assert_eq!(list("Fn+,"), "Fn+,");
        assert_eq!(list("Fn+, , F13"), "Fn+,, F13");
        assert_eq!(list("Fn + , , F13"), "Fn+,, F13");
        assert_eq!(list("Fn+，，F13"), "Fn+,, F13");
        // The comma key needs a separator after it before the next combo.
        assert!(matches!(
            parse_list("Fn+, F13"),
            Err(ParseError::UnknownKey { .. })
        ));
        assert_eq!(list("Fn, fn, function"), "Fn");
        assert_eq!(list("option+command, Super+Alt, F9"), "Alt+Super, F9");
        assert_eq!(list(",F9,,"), "F9");
        assert_eq!(parse_list(""), Ok(vec![]));
        assert_eq!(parse_list(" , "), Ok(vec![]));
        assert_eq!(parse_list("，"), Ok(vec![]));
        assert_eq!(
            parse_list("F9, Ctrl+Nope, F13"),
            Err(ParseError::UnknownKey {
                part: "Nope".into(),
                input: "Ctrl+Nope".into()
            })
        );
        let combos = vec![parse("Fn+,"), parse("Alt+Super"), parse("F13")];
        assert_eq!(parse_list(&format_list(&combos)), Ok(combos));
    }

    #[test]
    fn global_hotkeys_refuse_text_keys_and_fn_with_function_keys() {
        for ok in [
            "Alt+Super",
            "F9",
            "Alt+F8",
            "Ctrl+Alt+Tab",
            "Fn+A",
            "Fn+,",
            "Fn",
            "Menu",
            "Escape",
        ] {
            assert_eq!(parse(ok).check_global(), Ok(()), "{ok}");
        }
        for text in [
            "Ctrl+A",
            "Shift+Space",
            "A",
            "1",
            "Num5",
            "Alt+/",
            "Ctrl+Alt+`",
        ] {
            let c = parse(text);
            assert_eq!(c.check_global(), Err(Disallowed::TextKey(c)), "{text}");
        }
        for f in ["Fn+F1", "Fn+F12", "Ctrl+Fn+F24"] {
            let c = parse(f);
            assert_eq!(
                c.check_global(),
                Err(Disallowed::FnWithFunctionKey(c)),
                "{f}"
            );
        }
        assert_eq!(
            Combo::new(Mods::NONE, Key::None).check_global(),
            Err(Disallowed::Empty)
        );
    }
}
