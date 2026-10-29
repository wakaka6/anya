//! What the overlay shows at a given moment: the view it was handed, the
//! hold after a finished session, and the meter's smoothing. Time comes in
//! as an argument, so every rule here is tested without a display.

use std::f32::consts::TAU;
use std::mem::discriminant;
use std::time::{Duration, Instant};

use crate::{Labels, Style, View};

/// How long a finished session's result stays before the overlay goes idle.
pub(super) const DONE_HOLD: Duration = Duration::from_millis(1200);
/// The frame interval while the meter or the pulse moves.
pub(super) const FRAME: Duration = Duration::from_millis(33);
/// Bars in the meter.
pub(super) const BARS: usize = 5;

/// How much of the level each bar shows: tallest in the middle.
const WEIGHTS: [f32; BARS] = [0.6, 0.9, 1.0, 0.85, 0.65];
/// How fast each bar wobbles, in radians per second.
const SPEEDS: [f32; BARS] = [5.0, 6.7, 4.1, 7.6, 5.9];
/// Time constants of the level smoothing, rising and falling, in seconds.
const RISE: f32 = 0.05;
const FALL: f32 = 0.18;
/// The longest step the smoothing takes at once, so a stalled thread does
/// not jump.
const MAX_STEP: f32 = 0.1;
/// One pulse of the finishing dot, in seconds.
const PULSE: f32 = 1.4;

/// A colour with straight (not premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rgba {
    pub(super) r: u8,
    pub(super) g: u8,
    pub(super) b: u8,
    pub(super) a: u8,
}

impl Rgba {
    const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r, g, b, a: 255 }
    }

    const fn alpha(self, a: u8) -> Rgba {
        Rgba { a, ..self }
    }
}

/// The capsule's fill.
pub(super) const BACKGROUND: Rgba = Rgba::rgb(20, 20, 20).alpha(224);
const TEXT: Rgba = Rgba::rgb(245, 245, 245);
const DIM: Rgba = TEXT.alpha(128);
const RECORDING: Rgba = Rgba::rgb(255, 69, 58);
const FINISHING: Rgba = Rgba::rgb(255, 149, 0);
const DONE: Rgba = Rgba::rgb(52, 199, 89);
const IDLE: Rgba = Rgba::rgb(145, 145, 145);
const ERROR_TEXT: Rgba = Rgba::rgb(255, 158, 148);

/// The mark at the capsule's left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Icon {
    /// A round status dot.
    Dot(Rgba),
    /// A check mark: the session's text went somewhere.
    Check(Rgba),
}

/// The level meter: each bar's height from 0 (flat) to 1 (full).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Meter {
    pub(super) bars: [f32; BARS],
    pub(super) color: Rgba,
}

/// Which end of a line too long to fit survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Keep {
    /// The beginning, cut with a trailing ellipsis: messages and labels.
    Start,
    /// The end, cut with a leading ellipsis: a growing transcript.
    End,
}

/// One run of text.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Line {
    pub(super) text: String,
    pub(super) color: Rgba,
    pub(super) keep: Keep,
}

/// What to draw, independent of pixels. Equal scenes draw the same pixels,
/// so the driver skips them.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Scene {
    /// Counts sessions: the window picks its monitor again for a new one,
    /// and the capsule only grows within one.
    pub(super) session: u64,
    /// The idle marker: as narrow as its content allows.
    pub(super) idle: bool,
    pub(super) icon: Icon,
    pub(super) meter: Option<Meter>,
    /// The main text.
    pub(super) text: Option<Line>,
    /// A smaller, dimmer text after it: where the result went, a hint.
    pub(super) note: Option<Line>,
}

/// The overlay's state between frames.
pub(super) struct Model {
    view: View,
    /// When the current kind of view began.
    since: Instant,
    /// While a finished session's result holds.
    hold_until: Option<Instant>,
    session: u64,
    level: f32,
    target: f32,
    bars: [f32; BARS],
    phase: f32,
    last: Instant,
}

impl Model {
    /// Idle, at `now`.
    pub(super) fn new(now: Instant) -> Model {
        Model {
            view: View::Idle,
            since: now,
            hold_until: None,
            session: 0,
            level: 0.0,
            target: 0.0,
            bars: [0.0; BARS],
            phase: 0.0,
            last: now,
        }
    }

    /// Takes the view the app sends. The same kind again updates in place;
    /// [`View::Idle`] waits for a held result to run out.
    pub(super) fn show(&mut self, view: View, now: Instant) {
        if matches!(view, View::Idle) && self.hold_until.is_some_and(|t| now < t) {
            return;
        }
        let same = discriminant(&self.view) == discriminant(&view);
        let new_session = match (&self.view, &view) {
            (_, View::Idle) => false,
            (View::Idle, _) => true,
            (View::Listening { .. } | View::Finishing { .. }, View::Listening { .. }) => false,
            (_, View::Listening { .. }) => true,
            _ => false,
        };
        if new_session {
            self.session += 1;
            self.level = 0.0;
            self.target = 0.0;
            self.bars = [0.0; BARS];
            self.phase = 0.0;
        }
        if !same {
            self.since = now;
        }
        self.hold_until = match view {
            View::Done { .. } if same => self.hold_until,
            View::Done { .. } => Some(now + DONE_HOLD),
            _ => None,
        };
        if let View::Listening { level, .. } = view {
            self.target = if level.is_finite() {
                level.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        self.view = view;
    }

    /// Advances time: ends a held result and moves the meter.
    pub(super) fn tick(&mut self, now: Instant) {
        let dt = now
            .saturating_duration_since(self.last)
            .as_secs_f32()
            .min(MAX_STEP);
        self.last = now;
        if self.hold_until.is_some_and(|t| now >= t) {
            self.hold_until = None;
            self.view = View::Idle;
            self.since = now;
        }
        if matches!(self.view, View::Listening { .. }) {
            let tau = if self.target > self.level { RISE } else { FALL };
            self.level += (self.target - self.level) * (1.0 - (-dt / tau).exp());
            self.phase += dt;
            self.bars = bars(self.level, self.phase);
        }
    }

    /// How long the driver may sleep before the next [`Model::tick`] changes
    /// something; `None` until the next view.
    pub(super) fn next_wake(&self, now: Instant) -> Option<Duration> {
        match self.view {
            View::Listening { .. } | View::Finishing { .. } => Some(FRAME),
            _ => self.hold_until.map(|t| t.saturating_duration_since(now)),
        }
    }

    /// What to draw now; `None` hides the overlay.
    pub(super) fn scene(&self, style: &Style, labels: &Labels, now: Instant) -> Option<Scene> {
        let words = |text: &str| {
            if style.show_text {
                line(text, TEXT, Keep::End)
            } else {
                None
            }
        };
        let scene = |idle, icon, meter, text, note| Scene {
            session: self.session,
            idle,
            icon,
            meter,
            text,
            note,
        };
        Some(match &self.view {
            View::Idle => {
                if !style.idle_visible {
                    return None;
                }
                let text = line(&labels.idle, DIM, Keep::Start);
                scene(true, Icon::Dot(IDLE), None, text, None)
            }
            View::Listening { text, .. } => {
                let meter = Meter {
                    bars: self.bars,
                    color: TEXT,
                };
                let text = words(text).or_else(|| line(&labels.listening, DIM, Keep::Start));
                scene(false, Icon::Dot(RECORDING), Some(meter), text, None)
            }
            View::Finishing { text } => {
                let t = now.saturating_duration_since(self.since).as_secs_f32();
                let pulse = 0.55 + 0.45 * (TAU * t / PULSE).cos();
                let dot = FINISHING.alpha((pulse * 255.0).round() as u8);
                let meter = Meter {
                    bars: self.bars,
                    color: DIM,
                };
                let text = words(text).or_else(|| line(&labels.recognizing, DIM, Keep::Start));
                scene(false, Icon::Dot(dot), Some(meter), text, None)
            }
            View::Done { text, label } => match words(text) {
                Some(text) => scene(
                    false,
                    Icon::Check(DONE),
                    None,
                    Some(text),
                    line(label, DIM, Keep::Start),
                ),
                None => scene(
                    false,
                    Icon::Check(DONE),
                    None,
                    line(label, TEXT, Keep::Start),
                    None,
                ),
            },
            // A failure is not recognized speech: it shows whatever show_text says.
            View::Error { message, hint } => scene(
                false,
                Icon::Dot(RECORDING),
                None,
                line(message, ERROR_TEXT, Keep::Start),
                line(hint, DIM, Keep::Start),
            ),
        })
    }
}

/// A line of `text`, or `None` when it is blank.
fn line(text: &str, color: Rgba, keep: Keep) -> Option<Line> {
    let text = text.trim();
    (!text.is_empty()).then(|| Line {
        text: text.to_owned(),
        color,
        keep,
    })
}

/// Each bar's height for a smoothed `level`: the square root lifts quiet
/// speech, the wobble keeps a steady tone alive, silence stays flat.
fn bars(level: f32, phase: f32) -> [f32; BARS] {
    let loud = level.max(0.0).sqrt();
    std::array::from_fn(|i| {
        let wobble = 0.8 + 0.2 * (phase * SPEEDS[i] + i as f32 * 1.3).sin();
        (loud * WEIGHTS[i] * wobble).clamp(0.0, 1.0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;

    fn style(show_text: bool, idle_visible: bool) -> Style {
        Style {
            position: Position::BottomCenter,
            scale: 1.0,
            idle_visible,
            show_text,
        }
    }

    fn labels() -> Labels {
        Labels {
            listening: "Listening…".into(),
            recognizing: "Recognizing…".into(),
            idle: "Idle".into(),
        }
    }

    fn listening(level: f32, text: &str) -> View {
        View::Listening {
            level,
            text: text.into(),
        }
    }

    fn done(text: &str) -> View {
        View::Done {
            text: text.into(),
            label: "Pasted".into(),
        }
    }

    fn texts(scene: &Scene) -> (Option<&str>, Option<&str>) {
        (
            scene.text.as_ref().map(|l| l.text.as_str()),
            scene.note.as_ref().map(|l| l.text.as_str()),
        )
    }

    #[test]
    fn a_result_holds_then_the_overlay_goes_idle_by_itself() {
        let t0 = Instant::now();
        let mut m = Model::new(t0);
        m.show(listening(0.5, "hi"), t0);
        m.show(done("hi"), t0);
        // The app reports idle right after; the result still holds.
        m.show(View::Idle, t0 + Duration::from_millis(100));
        m.tick(t0 + Duration::from_millis(1100));
        let s = m.scene(&style(true, false), &labels(), t0).expect("held");
        assert_eq!(s.icon, Icon::Check(DONE));
        assert_eq!(texts(&s), (Some("hi"), Some("Pasted")));
        assert_eq!(
            m.next_wake(t0 + Duration::from_millis(1100)),
            Some(Duration::from_millis(100))
        );
        m.tick(t0 + DONE_HOLD);
        assert!(m.scene(&style(true, false), &labels(), t0).is_none());
        let idle = m
            .scene(&style(true, true), &labels(), t0)
            .expect("idle marker");
        assert!(idle.idle);
        assert_eq!(texts(&idle), (Some("Idle"), None));
        assert_eq!(m.next_wake(t0 + DONE_HOLD), None);
    }

    #[test]
    fn a_new_recording_replaces_a_held_result_at_once() {
        let t0 = Instant::now();
        let mut m = Model::new(t0);
        m.show(done("one"), t0);
        let first = m.scene(&style(true, false), &labels(), t0).expect("done");
        m.show(listening(0.0, ""), t0 + Duration::from_millis(200));
        let s = m
            .scene(&style(true, false), &labels(), t0)
            .expect("listening");
        assert_eq!(s.icon, Icon::Dot(RECORDING));
        assert!(s.session > first.session);
        // The old hold no longer ends the new session.
        m.tick(t0 + DONE_HOLD * 2);
        assert!(m.scene(&style(true, false), &labels(), t0).is_some());
    }

    #[test]
    fn one_session_runs_from_listening_through_the_result() {
        let t0 = Instant::now();
        let mut m = Model::new(t0);
        let session = |m: &Model| {
            m.scene(&style(true, true), &labels(), t0)
                .expect("visible")
                .session
        };
        m.show(listening(0.1, ""), t0);
        let s1 = session(&m);
        m.show(listening(0.9, "a"), t0);
        m.show(View::Finishing { text: "ab".into() }, t0);
        m.show(listening(0.2, "ab"), t0);
        m.show(done("abc"), t0);
        m.show(done("abc"), t0 + Duration::from_millis(900));
        assert_eq!(session(&m), s1);
        // A repeated result does not restart the hold.
        m.tick(t0 + DONE_HOLD);
        assert!(
            m.scene(&style(true, true), &labels(), t0)
                .expect("idle")
                .idle
        );
        m.show(listening(0.0, ""), t0 + DONE_HOLD);
        assert_eq!(session(&m), s1 + 1);
        m.show(
            View::Error {
                message: "m".into(),
                hint: "h".into(),
            },
            t0 + DONE_HOLD,
        );
        assert_eq!(session(&m), s1 + 1);
        m.show(View::Idle, t0 + DONE_HOLD);
        m.show(
            View::Error {
                message: "m".into(),
                hint: "h".into(),
            },
            t0 + DONE_HOLD,
        );
        assert_eq!(session(&m), s1 + 2);
    }

    #[test]
    fn an_error_stays_until_the_next_view() {
        let t0 = Instant::now();
        let mut m = Model::new(t0);
        m.show(
            View::Error {
                message: "Connection refused".into(),
                hint: "Press R to retry".into(),
            },
            t0,
        );
        m.tick(t0 + Duration::from_secs(600));
        assert_eq!(m.next_wake(t0), None);
        let s = m.scene(&style(false, false), &labels(), t0).expect("error");
        assert_eq!(
            texts(&s),
            (Some("Connection refused"), Some("Press R to retry"))
        );
        assert_eq!(s.text.expect("message").color, ERROR_TEXT);
    }

    #[test]
    fn hiding_text_keeps_placeholders_labels_and_errors() {
        let t0 = Instant::now();
        let mut m = Model::new(t0);
        let hidden = style(false, false);
        let shown = style(true, false);
        m.show(listening(0.3, "secret words"), t0);
        let s = m.scene(&hidden, &labels(), t0).expect("listening");
        assert_eq!(texts(&s), (Some("Listening…"), None));
        let s = m.scene(&shown, &labels(), t0).expect("listening");
        assert_eq!(texts(&s), (Some("secret words"), None));
        assert_eq!(s.text.expect("text").keep, Keep::End);

        m.show(
            View::Finishing {
                text: "secret".into(),
            },
            t0,
        );
        let s = m.scene(&hidden, &labels(), t0).expect("finishing");
        assert_eq!(texts(&s), (Some("Recognizing…"), None));

        m.show(done("secret"), t0);
        let s = m.scene(&hidden, &labels(), t0).expect("done");
        assert_eq!(texts(&s), (Some("Pasted"), None));
        assert_eq!(s.text.expect("label").color, TEXT);

        // Listening with no text yet shows the placeholder even with text on.
        m.show(listening(0.0, "  "), t0 + DONE_HOLD);
        let s = m.scene(&shown, &labels(), t0).expect("listening");
        assert_eq!(texts(&s), (Some("Listening…"), None));
    }

    #[test]
    fn the_idle_marker_needs_idle_visible() {
        let t0 = Instant::now();
        let m = Model::new(t0);
        assert!(m.scene(&style(true, false), &labels(), t0).is_none());
        let s = m.scene(&style(true, true), &labels(), t0).expect("marker");
        assert_eq!(s.icon, Icon::Dot(IDLE));
        assert!(s.meter.is_none());
    }

    #[test]
    fn the_meter_follows_the_level_smoothly_and_silence_stays_flat() {
        let t0 = Instant::now();
        let mut m = Model::new(t0);
        m.show(listening(0.0, ""), t0);
        for i in 1..=30 {
            m.tick(t0 + FRAME * i);
        }
        assert_eq!(m.bars, [0.0; BARS]);

        m.show(listening(1.0, ""), t0 + FRAME * 30);
        m.tick(t0 + FRAME * 31);
        let first = m.level;
        for i in 32..=60 {
            m.tick(t0 + FRAME * i);
        }
        // It rises over several frames, not in one jump.
        assert!(first > 0.0 && first < 0.9, "{first}");
        assert!(
            m.level > first && m.level <= 1.0,
            "{first} then {}",
            m.level
        );
        assert!(m.bars.iter().all(|b| (0.0..=1.0).contains(b)));
        // The middle bar outweighs the outer ones.
        assert!(m.bars[2] > m.bars[0]);

        // Finishing freezes the bars where they are, and keeps pulsing.
        let frozen = m.bars;
        m.show(
            View::Finishing {
                text: String::new(),
            },
            t0 + FRAME * 60,
        );
        let a = m.scene(&style(true, false), &labels(), t0 + FRAME * 60);
        m.tick(t0 + FRAME * 80);
        let b = m.scene(&style(true, false), &labels(), t0 + FRAME * 80);
        assert_eq!(m.bars, frozen);
        assert_ne!(a.map(|s| s.icon), b.map(|s| s.icon));
        assert_eq!(m.next_wake(t0), Some(FRAME));
    }

    #[test]
    fn a_level_out_of_range_is_clamped() {
        let t0 = Instant::now();
        let mut m = Model::new(t0);
        m.show(listening(f32::NAN, ""), t0);
        assert_eq!(m.target, 0.0);
        m.show(listening(7.0, ""), t0);
        assert_eq!(m.target, 1.0);
    }
}
