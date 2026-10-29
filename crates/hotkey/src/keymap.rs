//! What each backend's key codes mean. Pure tables, so they are tested on
//! every platform.

use crate::matcher::{Phys, Side};
use crate::{Combo, Key, Mods, Reason};

/// A backend's codes and the physical keys they name.
pub(crate) type Table = &'static [(u32, Phys)];

const fn k(code: u32, key: Key) -> (u32, Phys) {
    (code, Phys::Key(key))
}

const fn m(code: u32, mods: Mods, side: Side) -> (u32, Phys) {
    (code, Phys::Mod(mods, side))
}

use Side::{Left as L, Right as R};

/// The physical key a code names.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(crate) fn lookup(table: Table, code: u32) -> Option<Phys> {
    table.iter().find(|(c, _)| *c == code).map(|&(_, p)| p)
}

/// Why `table`'s backend can never report `combo`, if it cannot.
pub(crate) fn unseen(table: Table, combo: Combo) -> Option<Reason> {
    for bit in [Mods::CTRL, Mods::ALT, Mods::SHIFT, Mods::SUPER, Mods::FN] {
        let seen = table
            .iter()
            .any(|&(_, p)| matches!(p, Phys::Mod(m, _) if m == bit));
        if combo.mods.contains(bit) && !seen {
            return Some(Reason::ModifierUnseen(bit));
        }
    }
    if combo.key != Key::None && !table.iter().any(|&(_, p)| p == Phys::Key(combo.key)) {
        return Some(Reason::KeyUnseen(combo.key));
    }
    None
}

/// macOS virtual key codes (`kVK_*`). CapsLock reports only its lock state
/// and Mac keyboards have no F21 to F24, so those are missing.
#[cfg(any(target_os = "macos", test))]
#[rustfmt::skip]
pub(crate) const MAC: Table = &[
    k(0x00, Key::A), k(0x01, Key::S), k(0x02, Key::D), k(0x03, Key::F),
    k(0x04, Key::H), k(0x05, Key::G), k(0x06, Key::Z), k(0x07, Key::X),
    k(0x08, Key::C), k(0x09, Key::V), k(0x0B, Key::B), k(0x0C, Key::Q),
    k(0x0D, Key::W), k(0x0E, Key::E), k(0x0F, Key::R), k(0x10, Key::Y),
    k(0x11, Key::T), k(0x12, Key::Digit1), k(0x13, Key::Digit2), k(0x14, Key::Digit3),
    k(0x15, Key::Digit4), k(0x16, Key::Digit6), k(0x17, Key::Digit5), k(0x18, Key::Equal),
    k(0x19, Key::Digit9), k(0x1A, Key::Digit7), k(0x1B, Key::Minus), k(0x1C, Key::Digit8),
    k(0x1D, Key::Digit0), k(0x1E, Key::RightBracket), k(0x1F, Key::O), k(0x20, Key::U),
    k(0x21, Key::LeftBracket), k(0x22, Key::I), k(0x23, Key::P), k(0x24, Key::Enter),
    k(0x25, Key::L), k(0x26, Key::J), k(0x27, Key::Quote), k(0x28, Key::K),
    k(0x29, Key::Semicolon), k(0x2A, Key::Backslash), k(0x2B, Key::Comma), k(0x2C, Key::Slash),
    k(0x2D, Key::N), k(0x2E, Key::M), k(0x2F, Key::Period), k(0x30, Key::Tab),
    k(0x31, Key::Space), k(0x32, Key::Backtick), k(0x33, Key::Backspace), k(0x35, Key::Escape),
    m(0x36, Mods::SUPER, R), m(0x37, Mods::SUPER, L), m(0x38, Mods::SHIFT, L),
    m(0x3A, Mods::ALT, L), m(0x3B, Mods::CTRL, L), m(0x3C, Mods::SHIFT, R),
    m(0x3D, Mods::ALT, R), m(0x3E, Mods::CTRL, R), m(0x3F, Mods::FN, L),
    k(0x40, Key::F17), k(0x4F, Key::F18), k(0x50, Key::F19), k(0x5A, Key::F20),
    k(0x52, Key::Num0), k(0x53, Key::Num1), k(0x54, Key::Num2), k(0x55, Key::Num3),
    k(0x56, Key::Num4), k(0x57, Key::Num5), k(0x58, Key::Num6), k(0x59, Key::Num7),
    k(0x5B, Key::Num8), k(0x5C, Key::Num9),
    k(0x60, Key::F5), k(0x61, Key::F6), k(0x62, Key::F7), k(0x63, Key::F3),
    k(0x64, Key::F8), k(0x65, Key::F9), k(0x67, Key::F11), k(0x69, Key::F13),
    k(0x6A, Key::F16), k(0x6B, Key::F14), k(0x6D, Key::F10), k(0x6E, Key::Menu),
    k(0x6F, Key::F12), k(0x71, Key::F15), k(0x72, Key::Insert), k(0x73, Key::Home),
    k(0x74, Key::PageUp), k(0x75, Key::Delete), k(0x76, Key::F4), k(0x77, Key::End),
    k(0x78, Key::F2), k(0x79, Key::PageDown), k(0x7A, Key::F1), k(0x7B, Key::Left),
    k(0x7C, Key::Right), k(0x7D, Key::Down), k(0x7E, Key::Up),
];

/// Whether a macOS `FlagsChanged` event for modifier key `p` means it went
/// down. `flags` are the event's `CGEventFlags`: the device bits tell left
/// from right; events without them (posted by software) fall back to the
/// modifier's own bit.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn mac_modifier_down(p: Phys, flags: u64) -> bool {
    // NX_DEVICE*KEYMASK and kCGEventFlagMask*.
    const L_CTRL: u64 = 0x01;
    const L_SHIFT: u64 = 0x02;
    const R_SHIFT: u64 = 0x04;
    const L_CMD: u64 = 0x08;
    const R_CMD: u64 = 0x10;
    const L_ALT: u64 = 0x20;
    const R_ALT: u64 = 0x40;
    const R_CTRL: u64 = 0x2000;
    const SHIFT: u64 = 0x2_0000;
    const CONTROL: u64 = 0x4_0000;
    const ALTERNATE: u64 = 0x8_0000;
    const COMMAND: u64 = 0x10_0000;
    const SECONDARY_FN: u64 = 0x80_0000;

    let Phys::Mod(bit, side) = p else {
        return false;
    };
    let (any, left, right) = match bit {
        b if b == Mods::CTRL => (CONTROL, L_CTRL, R_CTRL),
        b if b == Mods::ALT => (ALTERNATE, L_ALT, R_ALT),
        b if b == Mods::SHIFT => (SHIFT, L_SHIFT, R_SHIFT),
        b if b == Mods::SUPER => (COMMAND, L_CMD, R_CMD),
        _ => return flags & SECONDARY_FN != 0,
    };
    if flags & any == 0 {
        return false;
    }
    let (this, other) = match side {
        Side::Left => (left, right),
        Side::Right => (right, left),
    };
    flags & this != 0 || flags & other == 0
}

/// The modifiers a macOS event's `CGEventFlags` say are held, among those
/// [`crate::matcher::Msg::Sync`] covers. Fn is left out: macOS sets its
/// flag on F-keys and arrows.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn mac_flag_mods(flags: u64) -> Mods {
    const BITS: [(u64, Mods); 4] = [
        (0x4_0000, Mods::CTRL),
        (0x8_0000, Mods::ALT),
        (0x2_0000, Mods::SHIFT),
        (0x10_0000, Mods::SUPER),
    ];
    let mut mods = Mods::NONE;
    for (bit, m) in BITS {
        if flags & bit != 0 {
            mods |= m;
        }
    }
    mods
}

/// Linux evdev key codes (`KEY_*`). Both the Compose and the Menu codes are
/// the Menu key.
#[cfg(any(target_os = "linux", test))]
#[rustfmt::skip]
pub(crate) const EVDEV: Table = &[
    k(1, Key::Escape), k(2, Key::Digit1), k(3, Key::Digit2), k(4, Key::Digit3),
    k(5, Key::Digit4), k(6, Key::Digit5), k(7, Key::Digit6), k(8, Key::Digit7),
    k(9, Key::Digit8), k(10, Key::Digit9), k(11, Key::Digit0), k(12, Key::Minus),
    k(13, Key::Equal), k(14, Key::Backspace), k(15, Key::Tab), k(16, Key::Q),
    k(17, Key::W), k(18, Key::E), k(19, Key::R), k(20, Key::T),
    k(21, Key::Y), k(22, Key::U), k(23, Key::I), k(24, Key::O),
    k(25, Key::P), k(26, Key::LeftBracket), k(27, Key::RightBracket), k(28, Key::Enter),
    m(29, Mods::CTRL, L), k(30, Key::A), k(31, Key::S), k(32, Key::D),
    k(33, Key::F), k(34, Key::G), k(35, Key::H), k(36, Key::J),
    k(37, Key::K), k(38, Key::L), k(39, Key::Semicolon), k(40, Key::Quote),
    k(41, Key::Backtick), m(42, Mods::SHIFT, L), k(43, Key::Backslash), k(44, Key::Z),
    k(45, Key::X), k(46, Key::C), k(47, Key::V), k(48, Key::B),
    k(49, Key::N), k(50, Key::M), k(51, Key::Comma), k(52, Key::Period),
    k(53, Key::Slash), m(54, Mods::SHIFT, R), m(56, Mods::ALT, L), k(57, Key::Space),
    k(58, Key::CapsLock), k(59, Key::F1), k(60, Key::F2), k(61, Key::F3),
    k(62, Key::F4), k(63, Key::F5), k(64, Key::F6), k(65, Key::F7),
    k(66, Key::F8), k(67, Key::F9), k(68, Key::F10),
    k(71, Key::Num7), k(72, Key::Num8), k(73, Key::Num9), k(75, Key::Num4),
    k(76, Key::Num5), k(77, Key::Num6), k(79, Key::Num1), k(80, Key::Num2),
    k(81, Key::Num3), k(82, Key::Num0), k(87, Key::F11), k(88, Key::F12),
    m(97, Mods::CTRL, R), m(100, Mods::ALT, R), k(102, Key::Home), k(103, Key::Up),
    k(104, Key::PageUp), k(105, Key::Left), k(106, Key::Right), k(107, Key::End),
    k(108, Key::Down), k(109, Key::PageDown), k(110, Key::Insert), k(111, Key::Delete),
    m(125, Mods::SUPER, L), m(126, Mods::SUPER, R), k(127, Key::Menu), k(139, Key::Menu),
    k(183, Key::F13), k(184, Key::F14), k(185, Key::F15), k(186, Key::F16),
    k(187, Key::F17), k(188, Key::F18), k(189, Key::F19), k(190, Key::F20),
    k(191, Key::F21), k(192, Key::F22), k(193, Key::F23), k(194, Key::F24),
    m(464, Mods::FN, L),
];

/// X11 keysyms. Fn has none, so X11 cannot see it. Meta counts as Alt, as
/// layouts put Meta on the Alt keys.
#[cfg(any(target_os = "linux", test))]
#[rustfmt::skip]
pub(crate) const X11: Table = &[
    k(0x61, Key::A), k(0x62, Key::B), k(0x63, Key::C), k(0x64, Key::D),
    k(0x65, Key::E), k(0x66, Key::F), k(0x67, Key::G), k(0x68, Key::H),
    k(0x69, Key::I), k(0x6a, Key::J), k(0x6b, Key::K), k(0x6c, Key::L),
    k(0x6d, Key::M), k(0x6e, Key::N), k(0x6f, Key::O), k(0x70, Key::P),
    k(0x71, Key::Q), k(0x72, Key::R), k(0x73, Key::S), k(0x74, Key::T),
    k(0x75, Key::U), k(0x76, Key::V), k(0x77, Key::W), k(0x78, Key::X),
    k(0x79, Key::Y), k(0x7a, Key::Z),
    k(0x41, Key::A), k(0x42, Key::B), k(0x43, Key::C), k(0x44, Key::D),
    k(0x45, Key::E), k(0x46, Key::F), k(0x47, Key::G), k(0x48, Key::H),
    k(0x49, Key::I), k(0x4a, Key::J), k(0x4b, Key::K), k(0x4c, Key::L),
    k(0x4d, Key::M), k(0x4e, Key::N), k(0x4f, Key::O), k(0x50, Key::P),
    k(0x51, Key::Q), k(0x52, Key::R), k(0x53, Key::S), k(0x54, Key::T),
    k(0x55, Key::U), k(0x56, Key::V), k(0x57, Key::W), k(0x58, Key::X),
    k(0x59, Key::Y), k(0x5a, Key::Z),
    k(0x30, Key::Digit0), k(0x31, Key::Digit1), k(0x32, Key::Digit2), k(0x33, Key::Digit3),
    k(0x34, Key::Digit4), k(0x35, Key::Digit5), k(0x36, Key::Digit6), k(0x37, Key::Digit7),
    k(0x38, Key::Digit8), k(0x39, Key::Digit9),
    k(0x20, Key::Space), k(0x60, Key::Backtick), k(0x2d, Key::Minus), k(0x3d, Key::Equal),
    k(0x5b, Key::LeftBracket), k(0x5d, Key::RightBracket), k(0x5c, Key::Backslash),
    k(0x3b, Key::Semicolon), k(0x27, Key::Quote), k(0x2c, Key::Comma), k(0x2e, Key::Period),
    k(0x2f, Key::Slash),
    k(0xffbe, Key::F1), k(0xffbf, Key::F2), k(0xffc0, Key::F3), k(0xffc1, Key::F4),
    k(0xffc2, Key::F5), k(0xffc3, Key::F6), k(0xffc4, Key::F7), k(0xffc5, Key::F8),
    k(0xffc6, Key::F9), k(0xffc7, Key::F10), k(0xffc8, Key::F11), k(0xffc9, Key::F12),
    k(0xffca, Key::F13), k(0xffcb, Key::F14), k(0xffcc, Key::F15), k(0xffcd, Key::F16),
    k(0xffce, Key::F17), k(0xffcf, Key::F18), k(0xffd0, Key::F19), k(0xffd1, Key::F20),
    k(0xffd2, Key::F21), k(0xffd3, Key::F22), k(0xffd4, Key::F23), k(0xffd5, Key::F24),
    k(0xff09, Key::Tab), k(0xfe20, Key::Tab), k(0xff0d, Key::Enter), k(0xff1b, Key::Escape),
    k(0xff08, Key::Backspace), k(0xffe5, Key::CapsLock),
    k(0xff50, Key::Home), k(0xff51, Key::Left), k(0xff52, Key::Up), k(0xff53, Key::Right),
    k(0xff54, Key::Down), k(0xff55, Key::PageUp), k(0xff56, Key::PageDown), k(0xff57, Key::End),
    k(0xff63, Key::Insert), k(0xffff, Key::Delete), k(0xff67, Key::Menu),
    k(0xffb0, Key::Num0), k(0xffb1, Key::Num1), k(0xffb2, Key::Num2), k(0xffb3, Key::Num3),
    k(0xffb4, Key::Num4), k(0xffb5, Key::Num5), k(0xffb6, Key::Num6), k(0xffb7, Key::Num7),
    k(0xffb8, Key::Num8), k(0xffb9, Key::Num9),
    m(0xffe1, Mods::SHIFT, L), m(0xffe2, Mods::SHIFT, R),
    m(0xffe3, Mods::CTRL, L), m(0xffe4, Mods::CTRL, R),
    m(0xffe9, Mods::ALT, L), m(0xffea, Mods::ALT, R),
    m(0xffe7, Mods::ALT, L), m(0xffe8, Mods::ALT, R),
    m(0xffeb, Mods::SUPER, L), m(0xffec, Mods::SUPER, R),
];

/// The physical key of each X keycode, from `GetKeyboardMapping`'s reply:
/// the first of a keycode's keysyms that names a key, so a shifted or
/// NumLock level still finds it (`KP_Home`/`KP_7` is Num7, `&`/`1` on a
/// French layout is 1).
#[cfg(any(target_os = "linux", test))]
pub(crate) fn x11_keycodes(
    min_keycode: u8,
    per_keycode: u8,
    keysyms: &[u32],
) -> std::collections::HashMap<u8, Phys> {
    let mut map = std::collections::HashMap::new();
    if per_keycode == 0 {
        return map;
    }
    for (i, syms) in keysyms.chunks(usize::from(per_keycode)).enumerate() {
        let Some(code) = u8::try_from(i)
            .ok()
            .and_then(|i| min_keycode.checked_add(i))
        else {
            break;
        };
        if let Some(p) = syms.iter().find_map(|&s| lookup(X11, s)) {
            map.insert(code, p);
        }
    }
    map
}

/// How many devices (or keycodes) hold each physical key, so two keyboards
/// holding Alt release it once the last lets go.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Default)]
pub(crate) struct Presses(std::collections::HashMap<Phys, u32>);

#[cfg(any(target_os = "linux", test))]
impl Presses {
    /// Counts a press; true when `p` was not held before.
    pub(crate) fn press(&mut self, p: Phys) -> bool {
        let n = self.0.entry(p).or_insert(0);
        *n += 1;
        *n == 1
    }

    /// Counts a release; true when nothing holds `p` any more.
    pub(crate) fn release(&mut self, p: Phys) -> bool {
        match self.0.get_mut(&p) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                self.0.remove(&p);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn phys_of(key: Key) -> Phys {
        Phys::Key(key)
    }

    #[test]
    fn linux_tables_see_every_key_and_x11_misses_only_fn() {
        for &key in Key::ALL {
            assert_eq!(unseen(EVDEV, Combo::key(key)), None, "evdev {key:?}");
            assert_eq!(unseen(X11, Combo::key(key)), None, "x11 {key:?}");
        }
        let all = Mods::CTRL | Mods::ALT | Mods::SHIFT | Mods::SUPER | Mods::FN;
        assert_eq!(unseen(EVDEV, Combo::new(all, Key::None)), None);
        assert_eq!(
            unseen(X11, Combo::new(Mods::FN, Key::Comma)),
            Some(Reason::ModifierUnseen(Mods::FN))
        );
        assert_eq!(
            unseen(X11, Combo::new(all.without(Mods::FN), Key::F24)),
            None
        );
    }

    #[test]
    fn macos_misses_capslock_and_f21_to_f24() {
        let missing: Vec<Key> = Key::ALL
            .iter()
            .copied()
            .filter(|&key| unseen(MAC, Combo::key(key)).is_some())
            .collect();
        assert_eq!(
            missing,
            [Key::F21, Key::F22, Key::F23, Key::F24, Key::CapsLock]
        );
        assert_eq!(unseen(MAC, Combo::new(Mods::FN, Key::None)), None);
        assert_eq!(
            unseen(MAC, Combo::new(Mods::ALT, Key::CapsLock)),
            Some(Reason::KeyUnseen(Key::CapsLock))
        );
    }

    #[test]
    fn codes_are_unique_and_known_codes_map() {
        for table in [MAC, EVDEV, X11] {
            let codes: HashSet<u32> = table.iter().map(|&(c, _)| c).collect();
            assert_eq!(codes.len(), table.len());
        }
        assert_eq!(lookup(MAC, 0x6E), Some(phys_of(Key::Menu)));
        assert_eq!(lookup(MAC, 0x3F), Some(Phys::Mod(Mods::FN, L)));
        assert_eq!(lookup(MAC, 0x36), Some(Phys::Mod(Mods::SUPER, R)));
        assert_eq!(lookup(MAC, 0x39), None, "CapsLock");
        assert_eq!(lookup(EVDEV, 464), Some(Phys::Mod(Mods::FN, L)));
        assert_eq!(lookup(EVDEV, 127), Some(phys_of(Key::Menu)));
        assert_eq!(lookup(EVDEV, 183), Some(phys_of(Key::F13)));
        assert_eq!(lookup(X11, 0xffe7), Some(Phys::Mod(Mods::ALT, L)));
        assert_eq!(lookup(X11, 0xfe03), None, "AltGr is not Alt");
    }

    #[test]
    fn macos_modifier_flags_tell_sides_apart() {
        let alt_l = Phys::Mod(Mods::ALT, L);
        let alt_r = Phys::Mod(Mods::ALT, R);
        // Left Alt down, then right Alt down, then left up, then right up.
        assert!(mac_modifier_down(alt_l, 0x8_0020));
        assert!(mac_modifier_down(alt_r, 0x8_0060));
        assert!(!mac_modifier_down(alt_l, 0x8_0040));
        assert!(!mac_modifier_down(alt_r, 0x0));
        // Posted by software: no device bits, the Alternate bit decides.
        assert!(mac_modifier_down(alt_r, 0x8_0000));
        assert!(!mac_modifier_down(alt_r, 0x10_0000));
        let fn_key = Phys::Mod(Mods::FN, L);
        assert!(mac_modifier_down(fn_key, 0x80_0000));
        assert!(!mac_modifier_down(fn_key, 0x0));
        // Flags on any event: Shift and Command held, Fn's bit ignored.
        assert_eq!(mac_flag_mods(0x92_0000), Mods::SHIFT | Mods::SUPER);
        assert_eq!(mac_flag_mods(0x80_0000), Mods::NONE);
    }

    #[test]
    fn x11_keycodes_take_the_first_level_that_names_a_key() {
        // Keycodes 10..=13, two keysyms each.
        let syms = [
            0x26, 0x31, // ampersand, 1 (French digit row)
            0xff95, 0xffb7, // KP_Home, KP_7
            0xffe9, 0xffe7, // Alt_L, Meta_L
            0x10006c1, 0x0, // a Cyrillic letter, NoSymbol
        ];
        let map = x11_keycodes(10, 2, &syms);
        assert_eq!(map.get(&10), Some(&phys_of(Key::Digit1)));
        assert_eq!(map.get(&11), Some(&phys_of(Key::Num7)));
        assert_eq!(map.get(&12), Some(&Phys::Mod(Mods::ALT, L)));
        assert_eq!(map.get(&13), None);
        assert!(x11_keycodes(8, 0, &syms).is_empty());
    }

    #[test]
    fn a_key_held_twice_is_released_by_the_last_release() {
        let alt = Phys::Mod(Mods::ALT, L);
        let mut p = Presses::default();
        assert!(p.press(alt));
        assert!(!p.press(alt));
        assert!(!p.release(alt));
        assert!(p.release(alt));
        assert!(!p.release(alt), "a release never seen pressed");
    }
}
