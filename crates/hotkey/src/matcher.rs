//! Turns physical key edges into combo edges. Pure state, driven by one
//! thread that every backend feeds.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::{Combo, Edge, Event, Key, Mods};

/// Which of a modifier's two keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    /// The left key (Fn's only key).
    Left,
    /// The right key.
    Right,
}

/// A physical key a backend reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Phys {
    /// A key that is not a modifier.
    Key(Key),
    /// One key of a modifier (a single bit).
    Mod(Mods, Side),
}

/// What the matcher produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Out {
    /// A registered combo went down or up.
    Edge(Event),
    /// A recording ended: the combo, or `None` when cancelled.
    Recorded(Option<Combo>),
}

#[derive(Debug, Default)]
struct Recording {
    /// A modifier went down since recording started.
    mod_pressed: bool,
}

/// Combo matching over held physical keys.
#[derive(Debug, Default)]
pub(crate) struct Matcher {
    combos: Vec<Combo>,
    active: Vec<Combo>,
    held: Vec<Phys>,
    recording: Option<Recording>,
}

impl Matcher {
    fn mods(&self) -> Mods {
        let mut mods = Mods::NONE;
        for p in &self.held {
            if let Phys::Mod(m, _) = *p {
                mods |= m;
            }
        }
        mods
    }

    fn edge(combo: Combo, edge: Edge) -> Out {
        Out::Edge(Event { combo, edge })
    }

    /// Replaces the registered combos. A removed combo that is held goes up;
    /// an added one fires on its next press.
    pub(crate) fn set(&mut self, combos: Vec<Combo>) -> Vec<Out> {
        let mut out = Vec::new();
        self.active.retain(|c| {
            let keep = combos.contains(c);
            if !keep {
                out.push(Matcher::edge(*c, Edge::Up));
            }
            keep
        });
        self.combos = combos;
        out
    }

    /// Starts recording; a recording already running is superseded.
    pub(crate) fn record(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        if self.recording.is_some() {
            out.push(Out::Recorded(None));
        }
        out.extend(self.release_all());
        self.recording = Some(Recording::default());
        out
    }

    /// Stops recording without a result (its caller went away).
    pub(crate) fn stop_recording(&mut self) {
        self.recording = None;
    }

    /// Whether a recording is running.
    pub(crate) fn recording(&self) -> bool {
        self.recording.is_some()
    }

    /// A physical key went down.
    pub(crate) fn down(&mut self, p: Phys) -> Vec<Out> {
        if self.held.contains(&p) {
            // Auto-repeat, or a second report of a held key.
            return Vec::new();
        }
        self.held.push(p);
        let mods = self.mods();

        if let Some(rec) = &mut self.recording {
            return match p {
                Phys::Mod(..) => {
                    rec.mod_pressed = true;
                    Vec::new()
                }
                Phys::Key(Key::Escape) if mods.is_empty() => {
                    self.recording = None;
                    vec![Out::Recorded(None)]
                }
                Phys::Key(key) => {
                    self.recording = None;
                    vec![Out::Recorded(Some(Combo::new(mods, key)))]
                }
            };
        }

        let mut out = Vec::new();
        for &c in &self.combos {
            if self.active.contains(&c) || c.is_empty() || c.mods != mods {
                continue;
            }
            let fires = match (c.key, p) {
                // Modifiers alone: the press of one of them completes it.
                (Key::None, Phys::Mod(m, _)) => c.mods.contains(m),
                (Key::None, Phys::Key(_)) => false,
                // A key with modifiers: the key's press, or a modifier's
                // press while the key is held.
                (key, Phys::Key(k)) => key == k,
                (key, Phys::Mod(m, _)) => c.mods.contains(m) && self.held.contains(&Phys::Key(key)),
            };
            if fires {
                self.active.push(c);
                out.push(Matcher::edge(c, Edge::Down));
            }
        }
        out
    }

    /// A physical key went up.
    pub(crate) fn up(&mut self, p: Phys) -> Vec<Out> {
        let Some(i) = self.held.iter().position(|h| *h == p) else {
            // Never seen down: held before listening started.
            return Vec::new();
        };
        let before = self.mods();
        self.held.remove(i);

        if let Some(rec) = &self.recording {
            if matches!(p, Phys::Mod(..)) && rec.mod_pressed {
                self.recording = None;
                return vec![Out::Recorded(Some(Combo::new(before, Key::None)))];
            }
            return Vec::new();
        }

        let mods = self.mods();
        let mut out = Vec::new();
        let held = &self.held;
        self.active.retain(|c| {
            let key_gone = c.key != Key::None && !held.contains(&Phys::Key(c.key));
            let broken = key_gone || !mods.contains(c.mods);
            if broken {
                out.push(Matcher::edge(*c, Edge::Up));
            }
            !broken
        });
        out
    }

    /// Forgets every held key (events may have been missed); held combos go
    /// up. A recording keeps running.
    pub(crate) fn reset(&mut self) -> Vec<Out> {
        self.held.clear();
        if let Some(rec) = &mut self.recording {
            rec.mod_pressed = false;
        }
        self.release_all()
    }

    /// Corrects the held Ctrl, Alt, Shift and Super to exactly `mods`, as an
    /// event's flags report them: a modifier held but not in `mods` is
    /// released, one in `mods` but not held is pressed (as its left key).
    pub(crate) fn sync(&mut self, mods: Mods) -> Vec<Out> {
        let mut out = Vec::new();
        for bit in SYNCED {
            let held: Vec<Phys> = self
                .held
                .iter()
                .copied()
                .filter(|p| matches!(p, Phys::Mod(m, _) if *m == bit))
                .collect();
            if mods.contains(bit) {
                if held.is_empty() {
                    out.extend(self.down(Phys::Mod(bit, Side::Left)));
                }
            } else {
                for p in held {
                    out.extend(self.up(p));
                }
            }
        }
        out
    }

    fn release_all(&mut self) -> Vec<Out> {
        self.active
            .drain(..)
            .map(|c| Matcher::edge(c, Edge::Up))
            .collect()
    }
}

/// What the matcher thread receives.
#[derive(Debug)]
pub(crate) enum Msg {
    /// A physical key went down.
    Down(Phys),
    /// A physical key went up.
    Up(Phys),
    /// Events may have been missed: forget held keys. Sent when macOS
    /// disabled the event tap for a while.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Reset,
    /// The held Ctrl, Alt, Shift and Super are exactly these: macOS events
    /// carry them as flags.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Sync(Mods),
    /// Replace the registered combos.
    Set(Vec<Combo>),
    /// Record the next combo.
    Record(oneshot::Sender<Option<Combo>>),
    /// The backend stopped.
    Closed,
}

/// How often a running recording checks whether its caller gave up.
const RECORDING_POLL: Duration = Duration::from_millis(100);

/// The modifiers [`Matcher::sync`] corrects.
const SYNCED: [Mods; 4] = [Mods::CTRL, Mods::ALT, Mods::SHIFT, Mods::SUPER];

/// Starts the matcher thread. It ends when every sender is gone or a
/// backend sends [`Msg::Closed`]; then `events` closes.
pub(crate) fn spawn(
    events: tokio_mpsc::UnboundedSender<Event>,
) -> std::io::Result<mpsc::Sender<Msg>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("anya-hotkey".into())
        .spawn(move || run(&rx, &events))?;
    Ok(tx)
}

fn run(rx: &mpsc::Receiver<Msg>, events: &tokio_mpsc::UnboundedSender<Event>) {
    let mut matcher = Matcher::default();
    let mut pending: Option<oneshot::Sender<Option<Combo>>> = None;
    loop {
        let msg = if matcher.recording() {
            match rx.recv_timeout(RECORDING_POLL) {
                Ok(msg) => Some(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => return,
            }
        };
        if pending.as_ref().is_some_and(oneshot::Sender::is_closed) {
            pending = None;
            matcher.stop_recording();
        }
        let Some(msg) = msg else { continue };
        let (out, next) = match msg {
            Msg::Down(p) => (matcher.down(p), None),
            Msg::Up(p) => (matcher.up(p), None),
            Msg::Reset => (matcher.reset(), None),
            Msg::Sync(mods) => (matcher.sync(mods), None),
            Msg::Set(combos) => (matcher.set(combos), None),
            Msg::Record(tx) => (matcher.record(), Some(tx)),
            Msg::Closed => return,
        };
        for o in out {
            match o {
                Out::Edge(e) => {
                    // The app may have dropped its receiver; recording still works.
                    let _ = events.send(e);
                }
                Out::Recorded(combo) => {
                    if let Some(tx) = pending.take() {
                        let _ = tx.send(combo);
                    }
                }
            }
        }
        if next.is_some() {
            pending = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Phys = Phys::Mod(Mods::CTRL, Side::Left);
    const ALT: Phys = Phys::Mod(Mods::ALT, Side::Left);
    const ALT_R: Phys = Phys::Mod(Mods::ALT, Side::Right);
    const SHIFT: Phys = Phys::Mod(Mods::SHIFT, Side::Left);
    const SUPER: Phys = Phys::Mod(Mods::SUPER, Side::Left);
    const FN: Phys = Phys::Mod(Mods::FN, Side::Left);

    fn key(k: Key) -> Phys {
        Phys::Key(k)
    }

    fn combo(s: &str) -> Combo {
        s.parse().expect("a combo")
    }

    fn with(combos: &[&str]) -> Matcher {
        let mut m = Matcher::default();
        let _ = m.set(combos.iter().map(|s| combo(s)).collect());
        m
    }

    fn down(c: &str) -> Out {
        Matcher::edge(combo(c), Edge::Down)
    }

    fn up(c: &str) -> Out {
        Matcher::edge(combo(c), Edge::Up)
    }

    #[test]
    fn modifiers_alone_fire_on_the_completing_press_and_release_on_the_first_up() {
        let mut m = with(&["Alt+Super"]);
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(SUPER), [down("Alt+Super")]);
        assert_eq!(m.down(SUPER), [], "a repeat fires nothing");
        assert_eq!(m.up(ALT), [up("Alt+Super")]);
        assert_eq!(m.up(SUPER), []);

        // Either press order.
        assert_eq!(m.down(SUPER), []);
        assert_eq!(m.down(ALT), [down("Alt+Super")]);
        assert_eq!(m.up(SUPER), [up("Alt+Super")]);
        assert_eq!(m.up(ALT), []);
    }

    #[test]
    fn modifiers_alone_need_exactly_their_modifiers() {
        let mut m = with(&["Alt+Super"]);
        assert_eq!(m.down(CTRL), []);
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(SUPER), [], "Ctrl is held too");
        assert_eq!(m.up(CTRL), [], "a release never completes a combo");
        assert_eq!(m.up(SUPER), []);
        assert_eq!(m.down(SUPER), [down("Alt+Super")]);
        // An extra modifier while held keeps it held; its required ones end it.
        assert_eq!(m.down(SHIFT), []);
        assert_eq!(m.up(SHIFT), []);
        assert_eq!(m.up(SUPER), [up("Alt+Super")]);
        assert_eq!(m.up(ALT), []);
    }

    #[test]
    fn left_and_right_keys_are_one_modifier() {
        let mut m = with(&["Alt"]);
        assert_eq!(m.down(ALT), [down("Alt")]);
        assert_eq!(m.down(ALT_R), [], "already held");
        assert_eq!(m.up(ALT), [], "right Alt still holds it");
        assert_eq!(m.up(ALT_R), [up("Alt")]);
        let mut m = with(&["Fn"]);
        assert_eq!(m.down(FN), [down("Fn")]);
        assert_eq!(m.up(FN), [up("Fn")]);
    }

    #[test]
    fn a_key_with_modifiers_fires_in_either_order_and_releases_on_any_member() {
        let mut m = with(&["Alt+R"]);
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(key(Key::R)), [down("Alt+R")]);
        assert_eq!(m.up(ALT), [up("Alt+R")], "the modifier released first");
        assert_eq!(m.up(key(Key::R)), []);

        assert_eq!(m.down(key(Key::R)), []);
        assert_eq!(
            m.down(ALT),
            [down("Alt+R")],
            "the modifier pressed after the key"
        );
        assert_eq!(m.up(key(Key::R)), [up("Alt+R")]);
        assert_eq!(m.up(ALT), []);

        // Extra or missing modifiers do not match.
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(SHIFT), []);
        assert_eq!(m.down(key(Key::R)), []);
        assert_eq!(m.up(SHIFT), [], "releasing never completes it");
        assert_eq!(m.up(key(Key::R)), []);
        assert_eq!(m.up(ALT), []);
    }

    #[test]
    fn a_key_alone_needs_no_modifier_held() {
        let mut m = with(&["Escape", "F13"]);
        assert_eq!(m.down(SHIFT), []);
        assert_eq!(m.down(key(Key::Escape)), [], "Shift+Esc is not Esc");
        assert_eq!(m.up(SHIFT), []);
        assert_eq!(m.up(key(Key::Escape)), []);
        assert_eq!(m.down(key(Key::F13)), [down("F13")]);
        assert_eq!(
            m.down(SHIFT),
            [],
            "a modifier pressed later does not end it"
        );
        assert_eq!(m.up(SHIFT), []);
        assert_eq!(m.up(key(Key::F13)), [up("F13")]);
    }

    #[test]
    fn fast_repeated_presses_alternate_strictly() {
        let mut m = with(&["F13", "Fn"]);
        for _ in 0..5 {
            assert_eq!(m.down(key(Key::F13)), [down("F13")]);
            assert_eq!(m.down(key(Key::F13)), [], "auto-repeat");
            assert_eq!(m.up(key(Key::F13)), [up("F13")]);
            assert_eq!(m.up(key(Key::F13)), [], "a stray release");
            assert_eq!(m.down(FN), [down("Fn")]);
            assert_eq!(m.up(FN), [up("Fn")]);
        }
    }

    #[test]
    fn several_hotkeys_go_down_and_up_on_their_own() {
        let mut m = with(&["F13", "Menu"]);
        assert_eq!(m.down(key(Key::F13)), [down("F13")]);
        assert_eq!(m.down(key(Key::Menu)), [down("Menu")]);
        assert_eq!(m.up(key(Key::F13)), [up("F13")]);
        assert_eq!(m.up(key(Key::Menu)), [up("Menu")]);
        assert_eq!(m.down(key(Key::Menu)), [down("Menu")]);
        assert_eq!(m.down(key(Key::F13)), [down("F13")]);
        assert_eq!(m.up(key(Key::Menu)), [up("Menu")]);
        assert_eq!(m.up(key(Key::F13)), [up("F13")]);
    }

    #[test]
    fn replacing_the_set_releases_only_removed_combos() {
        let mut m = with(&["F13", "Menu"]);
        assert_eq!(m.down(key(Key::F13)), [down("F13")]);
        assert_eq!(m.down(key(Key::Menu)), [down("Menu")]);
        assert_eq!(m.set(vec![combo("Menu"), combo("Escape")]), [up("F13")]);
        assert_eq!(m.up(key(Key::F13)), []);
        assert_eq!(m.up(key(Key::Menu)), [up("Menu")]);
        // Added back while held: it waits for a fresh press.
        assert_eq!(m.down(key(Key::F13)), []);
        assert_eq!(m.set(vec![combo("F13")]), []);
        assert_eq!(m.up(key(Key::F13)), []);
        assert_eq!(m.down(key(Key::F13)), [down("F13")]);
    }

    #[test]
    fn a_reset_releases_held_combos_and_forgets_held_keys() {
        let mut m = with(&["Alt+Super"]);
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(SUPER), [down("Alt+Super")]);
        assert_eq!(m.reset(), [up("Alt+Super")]);
        assert_eq!(m.up(ALT), []);
        assert_eq!(m.up(SUPER), []);
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(SUPER), [down("Alt+Super")]);
    }

    #[test]
    fn flags_correct_modifier_edges_that_never_arrived() {
        // Shift reported only as a flag on the key: F13 alone does not fire.
        let mut m = with(&["F13", "Shift+F13"]);
        assert_eq!(m.sync(Mods::SHIFT), []);
        assert_eq!(m.down(key(Key::F13)), [down("Shift+F13")]);
        assert_eq!(m.up(key(Key::F13)), [up("Shift+F13")]);
        assert_eq!(m.sync(Mods::NONE), []);
        assert_eq!(m.down(key(Key::F13)), [down("F13")]);
        assert_eq!(m.up(key(Key::F13)), [up("F13")]);

        // A modifier release that was missed: the next flags end the combo.
        let mut m = with(&["Alt+Super"]);
        assert_eq!(m.down(ALT_R), []);
        assert_eq!(m.down(SUPER), [down("Alt+Super")]);
        assert_eq!(m.sync(Mods::ALT | Mods::SUPER), [], "already held");
        assert_eq!(m.sync(Mods::SUPER), [up("Alt+Super")]);
        assert_eq!(m.sync(Mods::ALT | Mods::SUPER), [down("Alt+Super")]);
        // Fn is not among the flags synced.
        let mut m = with(&["Fn"]);
        assert_eq!(m.down(FN), [down("Fn")]);
        assert_eq!(m.sync(Mods::NONE), []);
    }

    #[test]
    fn recording_modifiers_alone_completes_on_the_first_release() {
        let mut m = with(&["Alt+Super"]);
        assert_eq!(m.record(), []);
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(SUPER), [], "paused while recording");
        assert_eq!(m.up(SUPER), [Out::Recorded(Some(combo("Alt+Super")))]);
        assert_eq!(m.up(ALT), [], "the rest of the release fires nothing");
        assert_eq!(m.down(ALT), []);
        assert_eq!(m.down(SUPER), [down("Alt+Super")], "hotkeys resume");

        let mut m = with(&[]);
        let _ = m.record();
        assert_eq!(m.down(FN), []);
        assert_eq!(m.up(FN), [Out::Recorded(Some(combo("Fn")))]);
    }

    #[test]
    fn recording_a_key_completes_on_its_press_and_esc_cancels() {
        let mut m = with(&[]);
        let _ = m.record();
        assert_eq!(m.down(CTRL), []);
        assert_eq!(m.down(ALT), []);
        assert_eq!(
            m.down(key(Key::Space)),
            [Out::Recorded(Some(combo("Ctrl+Alt+Space")))]
        );
        let _ = m.up(key(Key::Space));
        let _ = m.up(ALT);
        let _ = m.up(CTRL);

        let _ = m.record();
        assert_eq!(m.down(key(Key::Escape)), [Out::Recorded(None)]);
        let _ = m.up(key(Key::Escape));

        let _ = m.record();
        assert_eq!(m.down(SHIFT), []);
        assert_eq!(
            m.down(key(Key::Escape)),
            [Out::Recorded(Some(combo("Shift+Escape")))]
        );
    }

    #[test]
    fn recording_releases_held_combos_and_a_new_one_supersedes() {
        let mut m = with(&["F13"]);
        assert_eq!(m.down(key(Key::F13)), [down("F13")]);
        assert_eq!(m.record(), [up("F13")]);
        assert_eq!(m.up(key(Key::F13)), []);
        assert_eq!(m.record(), [Out::Recorded(None)]);
        // A modifier held before recording started does not complete it.
        let mut m = with(&[]);
        assert_eq!(m.down(ALT), []);
        let _ = m.record();
        assert_eq!(m.up(ALT), []);
        assert!(m.recording());
    }
}
