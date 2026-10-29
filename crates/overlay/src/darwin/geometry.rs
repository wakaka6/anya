//! The overlay's numbers, apart from AppKit so they can be tested: where the
//! notch is, how big each shape grows, where the capsule sits, how the meter
//! moves and how text is cut to fit.
//!
//! Coordinates are AppKit's: points, origin at the bottom left, y up.

use crate::Position;

/// A rectangle in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

impl Rect {
    pub(super) fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    pub(super) fn max_x(&self) -> f64 {
        self.x + self.w
    }

    pub(super) fn max_y(&self) -> f64 {
        self.y + self.h
    }

    pub(super) fn mid_x(&self) -> f64 {
        self.x + self.w / 2.0
    }

    /// Edges included, so the pointer on a screen's top row is on it.
    pub(super) fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.max_x() && y >= self.y && y <= self.max_y()
    }
}

/// What the overlay reads from one `NSScreen`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Screen {
    /// The whole screen.
    pub(super) frame: Rect,
    /// Without the menu bar and the Dock.
    pub(super) visible: Rect,
    /// `safeAreaInsets.top`: the housing's height on a notched screen, else 0.
    pub(super) safe_top: f64,
    /// Width of the menu bar area left of the housing.
    pub(super) aux_left: f64,
    /// Width of the menu bar area right of the housing.
    pub(super) aux_right: f64,
}

/// The camera housing the overlay grows out of. A screen without one gets
/// a virtual notch: no width, the menu bar's height, centered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Notch {
    pub(super) w: f64,
    pub(super) h: f64,
    /// Center, in screen coordinates.
    pub(super) cx: f64,
}

pub(super) fn notch(screen: &Screen) -> Notch {
    let f = screen.frame;
    let housing = f.w - screen.aux_left - screen.aux_right;
    if screen.safe_top > 0.0 && screen.aux_left > 0.0 && screen.aux_right > 0.0 && housing > 0.0 {
        Notch {
            w: housing,
            h: screen.safe_top,
            cx: f.x + screen.aux_left + housing / 2.0,
        }
    } else {
        Notch {
            w: 0.0,
            h: (f.max_y() - screen.visible.max_y()).max(0.0),
            cx: f.mid_x(),
        }
    }
}

/// Meter bars in the left ear.
pub(super) const BARS: usize = 5;
/// Text lines the notch shows at most.
pub(super) const MAX_LINES: u32 = 2;

/// The notch's sizes for one screen and scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Metrics {
    pub(super) s: f64,
    pub(super) notch: Notch,
    pub(super) ear_w: f64,
    pub(super) ear_pad: f64,
    pub(super) ears_h: f64,
    pub(super) icon_d: f64,
    pub(super) dot_d: f64,
    pub(super) bar_w: f64,
    pub(super) bar_gap: f64,
    pub(super) bar_min_h: f64,
    pub(super) bar_max_h: f64,
    pub(super) left_ear_w: f64,
    pub(super) right_ear_w: f64,
    pub(super) pad_x: f64,
    pub(super) text_body_w: f64,
    pub(super) line_h: f64,
    pub(super) flare: f64,
    pub(super) shake: f64,
}

/// One of the notch's shapes: the black body below the menu bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Shape {
    pub(super) w: f64,
    pub(super) h: f64,
    /// Radius of the concave corners that blend into the menu bar.
    pub(super) flare: f64,
    /// Radius of the rounded bottom corners.
    pub(super) bottom: f64,
}

/// How far the notch is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// Hidden behind the housing, or nothing on a virtual notch.
    Collapsed,
    /// The status strip: meter left of the housing, label right of it.
    Ears,
    /// The strip and the text below it.
    Text,
}

/// The panel on its screen, and where the notch is inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Layout {
    /// In screen coordinates.
    pub(super) panel: Rect,
    /// The notch's center x, in panel coordinates.
    pub(super) cx: f64,
    /// The panel's top edge, in panel coordinates: the screen's top.
    pub(super) top: f64,
}

impl Metrics {
    /// `line_h` is the height of one text line at this scale, measured by
    /// the font.
    pub(super) fn new(notch: Notch, scale: f64, line_h: f64) -> Metrics {
        let s = scale;
        let ear_w = (64.0 * s).round();
        let ear_pad = (14.0 * s).round();
        let ears_h = notch.h.max((24.0 * s).round());
        let bar_w = 3.0 * s;
        let bar_gap = 2.0 * s;
        Metrics {
            s,
            notch,
            ear_w,
            ear_pad,
            ears_h,
            icon_d: 11.0 * s,
            dot_d: 8.0 * s,
            bar_w,
            bar_gap,
            bar_min_h: 3.0 * s,
            bar_max_h: (16.0 * s).min(ears_h - 8.0),
            left_ear_w: 11.0 * s + 7.0 * s + BARS as f64 * bar_w + (BARS - 1) as f64 * bar_gap,
            right_ear_w: ear_w - ear_pad - 4.0 * s,
            pad_x: (18.0 * s).round(),
            text_body_w: (notch.w + 2.0 * ear_w).max(420.0 * s).round(),
            line_h,
            flare: 6.0 * s,
            shake: 10.0 * s,
        }
    }

    /// Height of the text area holding `lines` lines.
    pub(super) fn text_area_h(&self, lines: u32) -> f64 {
        if lines == 0 {
            0.0
        } else {
            2.0 * self.s + f64::from(lines) * self.line_h + 12.0 * self.s
        }
    }

    /// The panel holds the largest shape plus room for the error shake.
    pub(super) fn layout(&self, frame: Rect) -> Layout {
        let w = (self.text_body_w + 2.0 * self.flare + 2.0 * self.shake).ceil();
        let h = (self.ears_h + self.text_area_h(MAX_LINES) + 4.0).ceil();
        let x = (self.notch.cx - w / 2.0).round();
        Layout {
            panel: Rect::new(x, frame.max_y() - h, w, h),
            cx: self.notch.cx - x,
            top: h,
        }
    }

    /// The shape of `phase`. `lines` sizes the text phase; `extra` widens
    /// the ears when the right label is wider than its ear.
    pub(super) fn shape(&self, phase: Phase, lines: u32, extra: f64) -> Shape {
        match phase {
            Phase::Collapsed => {
                let h = if self.notch.w > 0.0 {
                    self.notch.h
                } else {
                    0.0
                };
                Shape {
                    w: self.notch.w,
                    h,
                    flare: 0.0,
                    bottom: (9.0f64).min(h / 2.0),
                }
            }
            Phase::Ears => Shape {
                w: (self.notch.w + 2.0 * self.ear_w + 2.0 * extra.max(0.0)).min(self.text_body_w),
                h: self.ears_h,
                flare: self.flare,
                bottom: (12.0 * self.s).min(self.ears_h - self.flare),
            },
            Phase::Text => {
                let h = self.ears_h + self.text_area_h(lines);
                Shape {
                    w: self.text_body_w,
                    h,
                    flare: self.flare,
                    bottom: (20.0 * self.s).min(h / 2.0),
                }
            }
        }
    }

    /// Center of the left ear (status icon and meter) for `shape`.
    pub(super) fn left_ear_x(&self, cx: f64, shape: &Shape) -> f64 {
        cx - shape.w / 2.0 + self.ear_pad + self.left_ear_w / 2.0
    }

    /// Center of the right label, `label_w` wide at least its ear, for `shape`.
    pub(super) fn right_label_x(&self, cx: f64, shape: &Shape, label_w: f64) -> f64 {
        cx + shape.w / 2.0 - self.ear_pad - label_w.max(self.right_ear_w) / 2.0
    }

    /// Vertical middle of the ear strip.
    pub(super) fn ears_mid_y(&self, top: f64) -> f64 {
        top - self.ears_h / 2.0
    }

    /// How much wider than its ear a `label_w` label makes the ears.
    pub(super) fn ears_extra(&self, label_w: f64) -> f64 {
        (label_w - self.right_ear_w).max(0.0)
    }

    /// The text area's rectangle for `lines` lines, in panel coordinates.
    pub(super) fn text_rect(&self, cx: f64, top: f64, lines: u32) -> Rect {
        let h = f64::from(lines) * self.line_h;
        Rect::new(
            cx - self.text_body_w / 2.0 + self.pad_x,
            top - self.ears_h - 2.0 * self.s - h,
            self.text_body_w - 2.0 * self.pad_x,
            h,
        )
    }

    /// Width the text wraps at.
    pub(super) fn text_w(&self) -> f64 {
        self.text_body_w - 2.0 * self.pad_x
    }

    /// Center x of meter bar `i`, inside the left ear.
    pub(super) fn bar_x(&self, i: usize) -> f64 {
        self.icon_d + 7.0 * self.s + i as f64 * (self.bar_w + self.bar_gap) + self.bar_w / 2.0
    }
}

/// One step of a path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum PathOp {
    Move(f64, f64),
    Line(f64, f64),
    Curve(f64, f64, f64, f64, f64, f64),
    Close,
}

/// The notch outline: concave flares at the top, straight sides, rounded
/// bottom corners. Every shape uses the same steps so Core Animation can
/// interpolate one into another.
pub(super) fn notch_path(cx: f64, top: f64, s: &Shape) -> [PathOp; 9] {
    const K: f64 = 0.552_284_749_8;
    let (l, r, b) = (cx - s.w / 2.0, cx + s.w / 2.0, top - s.h);
    let (fr, br) = (s.flare, s.bottom);
    [
        PathOp::Move(l - fr, top),
        PathOp::Curve(l - fr + K * fr, top, l, top - fr + K * fr, l, top - fr),
        PathOp::Line(l, b + br),
        PathOp::Curve(l, b + br - K * br, l + br - K * br, b, l + br, b),
        PathOp::Line(r - br, b),
        PathOp::Curve(r - br + K * br, b, r, b + br - K * br, r, b + br),
        PathOp::Line(r, top - fr),
        PathOp::Curve(r, top - fr + K * fr, r + fr - K * fr, top, r + fr, top),
        PathOp::Close,
    ]
}

/// The capsule's base sizes, before scale.
pub(super) const CAPSULE_H: f64 = 42.0;
pub(super) const CAPSULE_MARGIN: f64 = 28.0;
const CAPSULE_MIN_W: f64 = 122.0;
const CAPSULE_MAX_W: f64 = 420.0;
const CAPSULE_PAD: f64 = 16.0;
const CAPSULE_DOT: f64 = 14.0;
const CAPSULE_GAP: f64 = 14.0;
const CAPSULE_DETAIL_GAP: f64 = 8.0;

/// Where the capsule's parts go, in capsule coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CapsuleLayout {
    pub(super) w: f64,
    pub(super) h: f64,
    pub(super) dot: f64,
    /// Left edge of the dot.
    pub(super) dot_x: f64,
    pub(super) label_x: f64,
    pub(super) label_w: f64,
    pub(super) detail_x: f64,
    /// 0 when there is no detail or no room for it.
    pub(super) detail_w: f64,
}

/// Dot, label and detail centered in a pill at least the base width; the
/// label and then the detail give way past the maximum width.
pub(super) fn capsule_layout(scale: f64, label_w: f64, detail_w: f64) -> CapsuleLayout {
    let s = scale;
    let (pad, dot, gap, detail_gap) = (
        CAPSULE_PAD * s,
        CAPSULE_DOT * s,
        CAPSULE_GAP * s,
        CAPSULE_DETAIL_GAP * s,
    );
    let room = CAPSULE_MAX_W * s - 2.0 * pad - dot - gap;
    let label_w = label_w.clamp(0.0, room).ceil();
    let detail_room = room - label_w - detail_gap;
    let detail_w = if detail_w > 0.0 && detail_room >= 24.0 * s {
        detail_w.min(detail_room).ceil()
    } else {
        0.0
    };
    let content = dot
        + gap
        + label_w
        + if detail_w > 0.0 {
            detail_gap + detail_w
        } else {
            0.0
        };
    let w = (content + 2.0 * pad).max(CAPSULE_MIN_W * s).ceil();
    let dot_x = ((w - content) / 2.0).max(0.0);
    let label_x = dot_x + dot + gap;
    CapsuleLayout {
        w,
        h: (CAPSULE_H * s).ceil(),
        dot,
        dot_x,
        label_x,
        label_w,
        detail_x: label_x + label_w + detail_gap,
        detail_w,
    }
}

/// The capsule's origin inside a screen's visible frame. The notch
/// position, on a capsule, is the top center.
pub(super) fn capsule_origin(
    position: Position,
    visible: Rect,
    w: f64,
    h: f64,
    margin: f64,
) -> (f64, f64) {
    let left = visible.x + margin;
    let center = visible.x + (visible.w - w) / 2.0;
    let right = visible.max_x() - w - margin;
    let top = visible.max_y() - h - margin;
    let bottom = visible.y + margin;
    let (x, y) = match position {
        Position::TopLeft => (left, top),
        Position::Notch | Position::TopCenter => (center, top),
        Position::TopRight => (right, top),
        Position::BottomLeft => (left, bottom),
        Position::BottomCenter => (center, bottom),
        Position::BottomRight => (right, bottom),
    };
    (x.round(), y.round())
}

/// The index of the first screen under the pointer.
pub(super) fn screen_under(frames: &[Rect], x: f64, y: f64) -> Option<usize> {
    frames.iter().position(|f| f.contains(x, y))
}

/// Moves the meter's level toward `target`: quick to rise, slower to fall.
pub(super) fn smooth(current: f64, target: f64, dt: f64) -> f64 {
    let dt = dt.clamp(0.0, 0.1);
    let tau = if target > current { 0.05 } else { 0.18 };
    current + (target - current) * (1.0 - (-dt / tau).exp())
}

const BAR_WEIGHTS: [f64; BARS] = [0.55, 0.85, 1.0, 0.8, 0.6];
const BAR_SPEEDS: [f64; BARS] = [5.3, 7.1, 4.4, 6.2, 8.0];

/// Height of bar `i` at `level` (0 to 1), `phase` seconds into the wobble.
/// Silence stays flat at the minimum.
pub(super) fn bar_height(m: &Metrics, i: usize, level: f64, phase: f64) -> f64 {
    let wobble = 0.8 + 0.2 * (phase * BAR_SPEEDS[i] + i as f64 * 1.7).sin();
    let amount = (level.max(0.0).sqrt() * BAR_WEIGHTS[i] * wobble).min(1.0);
    m.bar_min_h + (m.bar_max_h - m.bar_min_h).max(0.0) * amount
}

/// Characters the typewriter reveals per tick with `backlog` still hidden:
/// a long append catches up in about half a second.
pub(super) fn type_step(backlog: usize) -> usize {
    1 + backlog / 16
}

/// `MM:SS` of the time recorded so far.
pub(super) fn clock(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// Text longer than this is cut to its tail before it is measured.
const MEASURED_CHARS: usize = 600;

/// The text as one line of words: newlines become spaces, the ends are
/// trimmed, and only the last [`MEASURED_CHARS`] characters are kept.
pub(super) fn prepare(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    let count = flat.chars().count();
    if count <= MEASURED_CHARS {
        return flat.to_owned();
    }
    let cut = flat
        .char_indices()
        .nth(count - MEASURED_CHARS)
        .map_or(flat.len(), |(i, _)| i);
    format!("…{}", &flat[boundary(flat, cut)..])
}

/// The newest part of `text` that `fits`, prefixed with `…` when its head
/// was dropped. The head is cut at a whole character, never inside one
/// drawn from several code points.
pub(super) fn fit_tail(text: &str, mut fits: impl FnMut(&str) -> bool) -> String {
    if fits(text) {
        return text.to_owned();
    }
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let at = |k: usize| starts.get(k).copied().unwrap_or(text.len());
    let candidate = |k: usize| format!("…{}", &text[at(k)..]);
    let (mut lo, mut hi) = (0, starts.len());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(&candidate(mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    format!("…{}", &text[boundary(text, at(hi))..])
}

/// Whether `c` draws as part of the character before it.
fn joins(prev: Option<char>, c: char) -> bool {
    prev == Some('\u{200D}')
        || matches!(
            c as u32,
            0x0300..=0x036F
                | 0x1AB0..=0x1AFF
                | 0x1DC0..=0x1DFF
                | 0x20D0..=0x20FF
                | 0xFE00..=0xFE0F
                | 0xFE20..=0xFE2F
                | 0x200D
                | 0x1F3FB..=0x1F3FF
                | 0xE0020..=0xE007F
                | 0xE0100..=0xE01EF
        )
}

fn regional(c: char) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&(c as u32))
}

/// The first offset at or after `at` that starts a whole drawn character:
/// past combining marks, variation selectors, skin tones, joined emoji and
/// the second half of a flag.
pub(super) fn boundary(text: &str, mut at: usize) -> usize {
    while let Some(c) = text[at..].chars().next() {
        let prev = text[..at].chars().next_back();
        let half_flag = regional(c)
            && text[..at]
                .chars()
                .rev()
                .take_while(|&p| regional(p))
                .count()
                % 2
                == 1;
        if !(joins(prev, c) || half_flag) {
            break;
        }
        at += c.len_utf8();
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 14-inch MacBook Pro: 1512×982, 32 pt housing 188 pt wide.
    fn notched() -> Screen {
        Screen {
            frame: Rect::new(0.0, 0.0, 1512.0, 982.0),
            visible: Rect::new(0.0, 0.0, 1512.0, 950.0),
            safe_top: 32.0,
            aux_left: 662.0,
            aux_right: 662.0,
        }
    }

    /// An external display right of it, with a 25 pt menu bar.
    fn external() -> Screen {
        Screen {
            frame: Rect::new(1512.0, -200.0, 2560.0, 1440.0),
            visible: Rect::new(1512.0, -200.0, 2560.0, 1415.0),
            safe_top: 0.0,
            aux_left: 0.0,
            aux_right: 0.0,
        }
    }

    #[test]
    fn the_notch_is_the_housing_between_the_menu_bar_areas() {
        assert_eq!(
            notch(&notched()),
            Notch {
                w: 188.0,
                h: 32.0,
                cx: 756.0
            }
        );
        let mut shifted = notched();
        shifted.frame.x = -1512.0;
        shifted.aux_left = 600.0;
        shifted.aux_right = 724.0;
        assert_eq!(notch(&shifted).cx, -1512.0 + 600.0 + 94.0);
    }

    #[test]
    fn a_screen_without_a_notch_gets_a_virtual_one_under_the_menu_bar() {
        assert_eq!(
            notch(&external()),
            Notch {
                w: 0.0,
                h: 25.0,
                cx: 1512.0 + 1280.0
            }
        );
        // A safe-area inset without menu bar areas beside it is no housing.
        let mut odd = external();
        odd.safe_top = 24.0;
        assert_eq!(notch(&odd).w, 0.0);
        // A hidden menu bar leaves nothing to grow out of but the edge.
        let mut hidden = external();
        hidden.visible = hidden.frame;
        assert_eq!(notch(&hidden).h, 0.0);
    }

    #[test]
    fn metrics_and_panel_follow_the_notch_and_scale() {
        let m = Metrics::new(notch(&notched()), 1.0, 16.0);
        assert_eq!((m.ear_w, m.ear_pad, m.ears_h), (64.0, 14.0, 32.0));
        assert_eq!(m.text_body_w, 420.0);
        assert_eq!(m.left_ear_w, 11.0 + 7.0 + 15.0 + 8.0);
        assert_eq!(m.bar_max_h, 16.0);
        assert_eq!(m.text_area_h(0), 0.0);
        assert_eq!(m.text_area_h(2), 2.0 + 32.0 + 12.0);

        let l = m.layout(notched().frame);
        assert_eq!(l.panel.w, 420.0 + 12.0 + 20.0);
        assert_eq!(l.panel.h, 32.0 + 46.0 + 4.0);
        assert_eq!(l.panel.x, 756.0 - 226.0);
        assert_eq!(l.panel.max_y(), 982.0, "flush with the screen's top");
        assert_eq!(l.cx, 226.0);
        assert_eq!(l.top, l.panel.h);

        let big = Metrics::new(notch(&notched()), 2.0, 32.0);
        assert_eq!((big.ear_w, big.ears_h), (128.0, 48.0));
        assert_eq!(big.text_body_w, 840.0);
        assert_eq!(big.bar_max_h, 32.0);
    }

    #[test]
    fn shapes_grow_from_the_housing() {
        let m = Metrics::new(notch(&notched()), 1.0, 16.0);
        let c = m.shape(Phase::Collapsed, 0, 0.0);
        assert_eq!((c.w, c.h, c.flare, c.bottom), (188.0, 32.0, 0.0, 9.0));
        let e = m.shape(Phase::Ears, 0, 0.0);
        assert_eq!((e.w, e.h, e.flare, e.bottom), (316.0, 32.0, 6.0, 12.0));
        let t = m.shape(Phase::Text, 2, 0.0);
        assert_eq!((t.w, t.h, t.bottom), (420.0, 32.0 + 46.0, 20.0));
        // A wide label widens the ears, never past the text body.
        assert_eq!(m.shape(Phase::Ears, 0, 20.0).w, 356.0);
        assert_eq!(m.shape(Phase::Ears, 0, 500.0).w, 420.0);
        assert_eq!(m.ears_extra(m.right_ear_w - 3.0), 0.0);
        assert_eq!(m.ears_extra(m.right_ear_w + 9.0), 9.0);

        let v = Metrics::new(notch(&external()), 1.0, 16.0);
        let vc = v.shape(Phase::Collapsed, 0, 0.0);
        assert_eq!(
            (vc.w, vc.h),
            (0.0, 0.0),
            "a virtual notch collapses to nothing"
        );
        assert_eq!(v.shape(Phase::Ears, 0, 0.0).w, 128.0);
        assert_eq!(v.ears_h, 25.0);
    }

    #[test]
    fn the_outline_is_closed_symmetric_and_hangs_from_the_top() {
        let shape = Shape {
            w: 316.0,
            h: 32.0,
            flare: 6.0,
            bottom: 12.0,
        };
        let path = notch_path(200.0, 80.0, &shape);
        assert_eq!(path[0], PathOp::Move(200.0 - 158.0 - 6.0, 80.0));
        assert_eq!(path[8], PathOp::Close);
        let PathOp::Curve(.., ex, ey) = path[7] else {
            panic!("ends on the right flare");
        };
        assert_eq!((ex, ey), (200.0 + 158.0 + 6.0, 80.0));
        assert_eq!(path[4], PathOp::Line(200.0 + 158.0 - 12.0, 48.0));
        // Same steps for every shape, so paths interpolate.
        let other = notch_path(
            200.0,
            80.0,
            &Shape {
                w: 0.0,
                h: 0.0,
                flare: 0.0,
                bottom: 0.0,
            },
        );
        for (a, b) in path.iter().zip(other.iter()) {
            assert_eq!(std::mem::discriminant(a), std::mem::discriminant(b));
        }
    }

    #[test]
    fn ears_and_text_sit_inside_the_shape() {
        let m = Metrics::new(notch(&notched()), 1.0, 16.0);
        let e = m.shape(Phase::Ears, 0, 0.0);
        assert_eq!(m.left_ear_x(226.0, &e), 226.0 - 158.0 + 14.0 + 20.5);
        assert_eq!(
            m.right_label_x(226.0, &e, 10.0),
            226.0 + 158.0 - 14.0 - m.right_ear_w / 2.0
        );
        assert_eq!(m.ears_mid_y(82.0), 66.0);
        let r = m.text_rect(226.0, 82.0, 2);
        assert_eq!(
            r,
            Rect::new(226.0 - 210.0 + 18.0, 82.0 - 32.0 - 2.0 - 32.0, 384.0, 32.0)
        );
        assert_eq!(m.bar_x(0), 11.0 + 7.0 + 1.5);
        assert_eq!(m.bar_x(4), 11.0 + 7.0 + 20.0 + 1.5);
    }

    #[test]
    fn the_capsule_sits_in_the_visible_frame() {
        let v = Rect::new(0.0, 80.0, 1512.0, 870.0);
        let (w, h, m) = (122.0, 42.0, 28.0);
        let at = |p| capsule_origin(p, v, w, h, m);
        assert_eq!(at(Position::TopLeft), (28.0, 950.0 - 42.0 - 28.0));
        assert_eq!(at(Position::TopCenter), (695.0, 880.0));
        assert_eq!(at(Position::Notch), at(Position::TopCenter));
        assert_eq!(at(Position::TopRight), (1512.0 - 122.0 - 28.0, 880.0));
        assert_eq!(at(Position::BottomLeft), (28.0, 108.0));
        assert_eq!(at(Position::BottomCenter), (695.0, 108.0));
        assert_eq!(at(Position::BottomRight), (1362.0, 108.0));
    }

    #[test]
    fn the_capsule_grows_with_its_words_up_to_a_limit() {
        let short = capsule_layout(1.0, 30.0, 0.0);
        assert_eq!((short.w, short.h), (122.0, 42.0));
        assert_eq!(short.detail_w, 0.0);
        let content = 14.0 + 14.0 + 30.0;
        assert_eq!(short.dot_x, (122.0 - content) / 2.0);
        assert_eq!(short.label_x, short.dot_x + 28.0);

        let long = capsule_layout(1.0, 60.0, 2000.0);
        assert_eq!(long.w, 420.0);
        assert_eq!(long.detail_x, long.label_x + 60.0 + 8.0);
        assert_eq!(long.detail_x + long.detail_w, 420.0 - 16.0);

        let scaled = capsule_layout(2.0, 30.0, 0.0);
        assert_eq!((scaled.w, scaled.h), (244.0, 84.0));
    }

    #[test]
    fn the_screen_under_the_pointer_wins() {
        let frames = [notched().frame, external().frame];
        assert_eq!(screen_under(&frames, 100.0, 100.0), Some(0));
        assert_eq!(screen_under(&frames, 2000.0, -100.0), Some(1));
        assert_eq!(screen_under(&frames, 100.0, 982.0), Some(0), "top row");
        assert_eq!(screen_under(&frames, 1512.0, 10.0), Some(0), "shared edge");
        assert_eq!(screen_under(&frames, -5.0, 10.0), None);
    }

    #[test]
    fn the_meter_rises_fast_falls_slow_and_rests_flat() {
        let up = smooth(0.0, 1.0, 1.0 / 60.0);
        let down = 1.0 - smooth(1.0, 0.0, 1.0 / 60.0);
        assert!(up > down, "attack {up} beats release {down}");
        assert_eq!(
            smooth(0.2, 0.9, 5.0),
            smooth(0.2, 0.9, 0.1),
            "dt is clamped"
        );
        assert!(smooth(0.0, 1.0, 0.1) < 1.0);

        let m = Metrics::new(notch(&notched()), 1.0, 16.0);
        for i in 0..BARS {
            for phase in [0.0, 0.3, 2.0] {
                assert_eq!(bar_height(&m, i, 0.0, phase), m.bar_min_h);
                let h = bar_height(&m, i, 1.0, phase);
                assert!(h > m.bar_min_h && h <= m.bar_max_h, "bar {i}: {h}");
            }
        }
        assert!(bar_height(&m, 2, 0.8, 0.0) > bar_height(&m, 2, 0.2, 0.0));
        assert!(
            bar_height(&m, 2, 0.5, 0.0) > bar_height(&m, 0, 0.5, 0.0),
            "middle bar is tallest"
        );
    }

    #[test]
    fn typing_speeds_up_with_the_backlog() {
        assert_eq!(type_step(0), 1);
        assert_eq!(type_step(15), 1);
        assert_eq!(type_step(16), 2);
        assert_eq!(type_step(160), 11);
    }

    #[test]
    fn the_clock_counts_minutes_and_seconds() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(59), "00:59");
        assert_eq!(clock(61), "01:01");
        assert_eq!(clock(6000), "100:00");
    }

    #[test]
    fn text_is_flattened_and_cut_to_its_tail() {
        assert_eq!(prepare("  one\ntwo\r\nthree  "), "one two  three");
        let long: String = std::iter::repeat_n('字', 700)
            .chain("end".chars())
            .collect();
        let cut = prepare(&long);
        assert!(cut.starts_with('…'));
        assert!(cut.ends_with("end"));
        assert_eq!(cut.chars().count(), 601);
    }

    #[test]
    fn the_tail_that_fits_keeps_the_newest_words() {
        let fits = |s: &str| s.chars().count() <= 10;
        assert_eq!(fit_tail("short", fits), "short");
        assert_eq!(fit_tail("abcdefghijklmnop", fits), "…hijklmnop");
        assert_eq!(
            fit_tail("你好世界你好世界你好世界", fits),
            "…界你好世界你好世界"
        );
        // When nothing but the ellipsis fits, that is what shows.
        assert_eq!(fit_tail("abc", |s| s == "…"), "…");
    }

    #[test]
    fn cuts_never_split_a_drawn_character() {
        let s = "a👩\u{200D}💻b";
        let zwj = s.find('\u{200D}').unwrap();
        assert_eq!(boundary(s, zwj), s.find('b').unwrap());
        let accent = "e\u{301}x";
        assert_eq!(boundary(accent, 1), 3);
        let flags = "🇨🇳🇯🇵";
        assert_eq!(boundary(flags, 4), 8, "second half of a flag");
        assert_eq!(boundary(flags, 8), 8, "a whole flag starts here");
        assert_eq!(boundary("ab", 2), 2);
        // A cut landing inside a joined emoji moves past it.
        let fits = |t: &str| t.chars().count() <= 3;
        assert_eq!(fit_tail("xx👩\u{200D}💻yy", fits), "…yy");
    }
}
