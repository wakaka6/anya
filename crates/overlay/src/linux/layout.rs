//! Sizes and places: where the window sits on a monitor, how wide the
//! capsule grows, where its parts go, and how long text is cut. Lengths in
//! logical pixels are scaled by the style's scale (and on Wayland the
//! output's buffer scale) into device pixels.

use super::model::{BARS, Keep, Scene};
use crate::Position;

/// The capsule's height.
pub(super) const HEIGHT: f64 = 42.0;
/// The gap between the capsule and the screen edges it is anchored to.
const MARGIN: f64 = 28.0;
/// The narrowest capsule outside idle.
const MIN_WIDTH: f64 = 122.0;
/// The widest capsule, and the window's width.
const MAX_WIDTH: f64 = 460.0;
const PAD: f32 = 16.0;
const ICON: f32 = 12.0;
const GAP: f32 = 10.0;
/// One meter bar's width.
pub(super) const BAR_WIDTH: f32 = 3.0;
const BAR_GAP: f32 = 2.5;
/// The meter's flat and full bar heights.
pub(super) const BAR_MIN: f32 = 3.0;
pub(super) const BAR_MAX: f32 = 18.0;
/// The main text's size.
const TEXT_PX: f32 = 15.0;
/// The note's size.
const NOTE_PX: f32 = 13.0;
/// The most of the room a note takes beside the main text.
const NOTE_SHARE: f32 = 0.4;
/// Characters of a line measured at most; a transcript can be long.
const MAX_CHARS: usize = 400;
const ELLIPSIS: char = '…';

/// The scale a style asks for, made safe: not positive or not a number is
/// 1.0, the rest is kept between 0.5 and 4.
pub(super) fn normalize_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale.clamp(0.5, 4.0)
    } else {
        1.0
    }
}

fn scaled(v: f64, scale: f64) -> u32 {
    (v * scale).round().max(1.0) as u32
}

/// The window's size in logical pixels: wide enough for the widest capsule.
pub(super) fn surface_size(scale: f64) -> (u32, u32) {
    (scaled(MAX_WIDTH, scale), scaled(HEIGHT, scale))
}

/// The gap to the anchored screen edges, in logical pixels.
pub(super) fn margin(scale: f64) -> i32 {
    scaled(MARGIN, scale) as i32
}

/// Where along an edge the capsule sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Align {
    Start,
    Center,
    End,
}

/// The screen edge and the place along it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Anchor {
    pub(super) top: bool,
    pub(super) align: Align,
}

/// Where `position` sits; Linux has no notch, so the notch is top center.
pub(super) fn anchor(position: Position) -> Anchor {
    let (top, align) = match position {
        Position::Notch | Position::TopCenter => (true, Align::Center),
        Position::TopLeft => (true, Align::Start),
        Position::TopRight => (true, Align::End),
        Position::BottomLeft => (false, Align::Start),
        Position::BottomCenter => (false, Align::Center),
        Position::BottomRight => (false, Align::End),
    };
    Anchor { top, align }
}

/// A rectangle in whole pixels: a monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rect {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) w: i32,
    pub(super) h: i32,
}

impl Rect {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// The top-left corner of a `size` window anchored on `monitor`, kept on it.
pub(super) fn place(anchor: Anchor, monitor: Rect, size: (u32, u32), margin: i32) -> (i32, i32) {
    let (w, h) = (size.0 as i32, size.1 as i32);
    let x = match anchor.align {
        Align::Start => monitor.x + margin,
        Align::Center => monitor.x + (monitor.w - w) / 2,
        Align::End => monitor.x + monitor.w - w - margin,
    };
    let y = if anchor.top {
        monitor.y + margin
    } else {
        monitor.y + monitor.h - h - margin
    };
    (x.max(monitor.x), y.max(monitor.y))
}

/// The monitor holding the pointer at `(x, y)`; the first one when none
/// does (a pointer between screens), `None` without monitors.
pub(super) fn monitor_at(monitors: &[Rect], x: i32, y: i32) -> Option<Rect> {
    monitors
        .iter()
        .copied()
        .find(|m| m.contains(x, y))
        .or_else(|| monitors.first().copied())
}

/// Measures text set in the overlay's font.
pub(super) trait Measure {
    /// The advance width of `text` at `px` pixels to the em.
    fn width(&mut self, text: &str, px: f32) -> f32;
}

/// `text` on one line within `max` pixels: whitespace runs become one
/// space, and a line too long loses the end `keep` does not name, marked
/// with an ellipsis. `None` when nothing fits or the text is blank.
pub(super) fn fit(
    m: &mut dyn Measure,
    text: &str,
    px: f32,
    max: f32,
    keep: Keep,
) -> Option<String> {
    let mut flat = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !flat.is_empty() {
            flat.push(' ');
        }
        flat.push_str(word);
    }
    if flat.is_empty() || max <= 0.0 {
        return None;
    }
    let count = flat.chars().count();
    if count > MAX_CHARS {
        flat = match keep {
            Keep::Start => flat.chars().take(MAX_CHARS).collect(),
            Keep::End => flat.chars().skip(count - MAX_CHARS).collect(),
        };
    } else if m.width(&flat, px) <= max {
        return Some(flat);
    }
    let bounds: Vec<usize> = flat
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(flat.len()))
        .collect();
    let chars = bounds.len() - 1;
    let cut = |n: usize| -> String {
        match keep {
            Keep::Start => format!("{}{ELLIPSIS}", flat[..bounds[n]].trim_end()),
            Keep::End => format!("{ELLIPSIS}{}", flat[bounds[chars - n]..].trim_start()),
        }
    };
    // The most characters that fit beside the ellipsis.
    let (mut lo, mut hi) = (0, chars);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if m.width(&cut(mid), px) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let line = cut(lo);
    (m.width(&line, px) <= max).then_some(line)
}

/// A rectangle in device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Area {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
}

/// Text placed on the capsule's middle line.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Placed {
    pub(super) text: String,
    pub(super) x: f32,
    pub(super) px: f32,
}

/// Where everything goes, in device pixels.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Layout {
    pub(super) pill: Area,
    /// The icon's center and diameter.
    pub(super) icon: (f32, f32, f32),
    pub(super) meter: Option<Area>,
    pub(super) text: Option<Placed>,
    pub(super) note: Option<Placed>,
    /// Device pixels per logical pixel.
    pub(super) k: f32,
}

/// Lays `scene` out on a `canvas` of device pixels with `k` device pixels
/// per logical pixel. The capsule is as wide as its content, at least as
/// wide as `floor` (what it reached earlier this session) and at most the
/// canvas, placed at `align`.
pub(super) fn layout(
    scene: &Scene,
    m: &mut dyn Measure,
    canvas: (u32, u32),
    k: f32,
    align: Align,
    floor: f32,
) -> Layout {
    let (cw, h) = (canvas.0 as f32, canvas.1 as f32);
    let (pad, icon, gap) = (PAD * k, ICON * k, GAP * k);
    let meter_w = if scene.meter.is_some() {
        BARS as f32 * BAR_WIDTH * k + (BARS - 1) as f32 * BAR_GAP * k
    } else {
        0.0
    };
    let mut used = 2.0 * pad + icon;
    if scene.meter.is_some() {
        used += gap + meter_w;
    }
    used += gap * (usize::from(scene.text.is_some()) + usize::from(scene.note.is_some())) as f32;
    let room = (cw - used).max(0.0);

    let (text_px, note_px) = (TEXT_PX * k, NOTE_PX * k);
    let note_room = if scene.text.is_some() {
        room * NOTE_SHARE
    } else {
        room
    };
    let note = scene
        .note
        .as_ref()
        .and_then(|n| fit(m, &n.text, note_px, note_room, n.keep));
    let note_w = note.as_deref().map_or(0.0, |t| m.width(t, note_px));
    let text = scene
        .text
        .as_ref()
        .and_then(|t| fit(m, &t.text, text_px, room - note_w, t.keep));
    let text_w = text.as_deref().map_or(0.0, |t| m.width(t, text_px));

    let mut content = 2.0 * pad + icon;
    if scene.meter.is_some() {
        content += gap + meter_w;
    }
    if text.is_some() {
        content += gap + text_w;
    }
    if note.is_some() {
        content += gap + note_w;
    }
    // The idle marker is not part of a session: it ignores the floor.
    let least = if scene.idle {
        h
    } else {
        (MIN_WIDTH as f32 * k).max(floor)
    };
    let w = content.max(least).min(cw).round();
    let x = match align {
        Align::Start => 0.0,
        Align::Center => ((cw - w) / 2.0).round(),
        Align::End => cw - w,
    };

    let mut cursor = x + ((w - content) / 2.0).max(0.0) + pad;
    let icon_at = (cursor + icon / 2.0, h / 2.0, icon);
    cursor += icon;
    let meter = scene.meter.map(|_| {
        cursor += gap;
        let area = Area {
            x: cursor,
            y: 0.0,
            w: meter_w,
            h,
        };
        cursor += meter_w;
        area
    });
    let mut place = |text: Option<String>, width: f32, px: f32| {
        text.map(|text| {
            cursor += gap;
            let placed = Placed {
                text,
                x: cursor,
                px,
            };
            cursor += width;
            placed
        })
    };
    let text = place(text, text_w, text_px);
    let note = place(note, note_w, note_px);
    Layout {
        pill: Area { x, y: 0.0, w, h },
        icon: icon_at,
        meter,
        text,
        note,
        k,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::linux::model::{Icon, Line, Meter, Rgba};

    /// Latin half an em wide, CJK a full em.
    pub(crate) struct Fake;

    impl Measure for Fake {
        fn width(&mut self, text: &str, px: f32) -> f32 {
            text.chars()
                .map(|c| if c >= '\u{2e80}' { px } else { px / 2.0 })
                .sum()
        }
    }

    const WHITE: Rgba = Rgba {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    fn scene(text: Option<&str>, note: Option<&str>, meter: bool) -> Scene {
        let line = |t: &str, keep| Line {
            text: t.into(),
            color: WHITE,
            keep,
        };
        Scene {
            session: 1,
            idle: false,
            icon: Icon::Dot(WHITE),
            meter: meter.then_some(Meter {
                bars: [0.0; BARS],
                color: WHITE,
            }),
            text: text.map(|t| line(t, Keep::End)),
            note: note.map(|t| line(t, Keep::Start)),
        }
    }

    #[test]
    fn scale_is_made_safe() {
        assert_eq!(normalize_scale(0.0), 1.0);
        assert_eq!(normalize_scale(-2.0), 1.0);
        assert_eq!(normalize_scale(f64::NAN), 1.0);
        assert_eq!(normalize_scale(f64::INFINITY), 1.0);
        assert_eq!(normalize_scale(1.5), 1.5);
        assert_eq!(normalize_scale(0.1), 0.5);
        assert_eq!(normalize_scale(40.0), 4.0);
    }

    #[test]
    fn sizes_follow_the_scale() {
        assert_eq!(surface_size(1.0), (460, 42));
        assert_eq!(surface_size(2.0), (920, 84));
        assert_eq!(surface_size(1.25), (575, 53));
        assert_eq!(margin(1.0), 28);
        assert_eq!(margin(1.5), 42);
    }

    #[test]
    fn every_position_lands_on_the_monitor_it_names() {
        let mon = Rect {
            x: 1920,
            y: 100,
            w: 1280,
            h: 800,
        };
        let size = (460, 42);
        let at = |p| place(anchor(p), mon, size, 28);
        assert_eq!(at(Position::TopLeft), (1948, 128));
        assert_eq!(at(Position::TopCenter), (1920 + 410, 128));
        assert_eq!(at(Position::Notch), at(Position::TopCenter));
        assert_eq!(at(Position::TopRight), (1920 + 1280 - 460 - 28, 128));
        assert_eq!(at(Position::BottomLeft), (1948, 100 + 800 - 42 - 28));
        assert_eq!(at(Position::BottomCenter), (2330, 830));
        assert_eq!(at(Position::BottomRight), (2712, 830));
        // At scale 2 the window and the margin double.
        let big = place(
            anchor(Position::BottomRight),
            mon,
            surface_size(2.0),
            margin(2.0),
        );
        assert_eq!(big, (1920 + 1280 - 920 - 56, 100 + 800 - 84 - 56));
        // A monitor too small keeps the window on it.
        let tiny = Rect {
            x: 0,
            y: 0,
            w: 300,
            h: 40,
        };
        assert_eq!(place(anchor(Position::BottomRight), tiny, size, 28), (0, 0));
    }

    #[test]
    fn the_monitor_under_the_pointer_is_picked() {
        let left = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let right = Rect {
            x: 1920,
            y: 0,
            w: 2560,
            h: 1440,
        };
        let both = [left, right];
        assert_eq!(monitor_at(&both, 100, 100), Some(left));
        assert_eq!(monitor_at(&both, 1920, 0), Some(right));
        assert_eq!(monitor_at(&both, 3000, 1200), Some(right));
        // Below the shorter left monitor: no one holds it, the first wins.
        assert_eq!(monitor_at(&both, 100, 1200), Some(left));
        assert_eq!(monitor_at(&[], 0, 0), None);
    }

    #[test]
    fn long_text_keeps_the_end_that_matters() {
        let f = &mut Fake;
        assert_eq!(
            fit(f, " hello \n world ", 10.0, 100.0, Keep::End).as_deref(),
            Some("hello world")
        );
        // 26 latin letters at 5 px is 130 px; 60 px holds the ellipsis and 11.
        let abc = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(
            fit(f, abc, 10.0, 60.0, Keep::End).as_deref(),
            Some("…pqrstuvwxyz")
        );
        assert_eq!(
            fit(f, abc, 10.0, 60.0, Keep::Start).as_deref(),
            Some("abcdefghijk…")
        );
        // CJK is cut at characters, a full em each.
        assert_eq!(
            fit(f, "今天天气很好我们出去走走", 10.0, 50.0, Keep::End).as_deref(),
            Some("…出去走走")
        );
        assert_eq!(fit(f, "   ", 10.0, 50.0, Keep::End), None);
        assert_eq!(fit(f, abc, 10.0, 4.0, Keep::End), None);
        // A transcript longer than what is measured keeps its last words.
        let long = format!("{}tail", "x".repeat(1000));
        let cut = fit(f, &long, 10.0, 50.0, Keep::End).expect("fits");
        assert!(cut.starts_with('…') && cut.ends_with("tail"), "{cut}");
    }

    #[test]
    fn the_capsule_grows_with_text_up_to_the_window() {
        let canvas = (460, 42);
        let short = layout(
            &scene(Some("hi"), None, true),
            &mut Fake,
            canvas,
            1.0,
            Align::Center,
            0.0,
        );
        assert_eq!(short.pill.w, 122.0);
        assert_eq!(short.pill.x, 169.0);
        assert_eq!(short.text.as_ref().map(|t| t.text.as_str()), Some("hi"));

        let words =
            "a fairly long sentence spoken into the microphone just now, still going on and on";
        let long = layout(
            &scene(Some(words), None, true),
            &mut Fake,
            canvas,
            1.0,
            Align::Center,
            0.0,
        );
        assert!(
            long.pill.w > 450.0 && long.pill.w <= 460.0,
            "{}",
            long.pill.w
        );
        let text = long.text.expect("text");
        assert!(text.text.starts_with('…') && text.text.ends_with("and on"));
        let end = text.x + Fake.width(&text.text, text.px);
        assert!(end <= long.pill.x + long.pill.w - 16.0 + 0.01, "{end}");

        // Earlier width in the same session is kept.
        let kept = layout(
            &scene(Some("hi"), None, true),
            &mut Fake,
            canvas,
            1.0,
            Align::Center,
            300.0,
        );
        assert_eq!(kept.pill.w, 300.0);
    }

    #[test]
    fn the_capsule_hugs_its_anchored_edge() {
        let s = scene(Some("hi"), None, false);
        let start = layout(&s, &mut Fake, (460, 42), 1.0, Align::Start, 0.0);
        let end = layout(&s, &mut Fake, (460, 42), 1.0, Align::End, 0.0);
        assert_eq!(start.pill.x, 0.0);
        assert_eq!(end.pill.x + end.pill.w, 460.0);
        assert!(start.meter.is_none());
    }

    #[test]
    fn the_idle_marker_is_narrow_and_scale_multiplies_everything() {
        let mut idle = scene(None, None, false);
        idle.idle = true;
        let l = layout(&idle, &mut Fake, (460, 42), 1.0, Align::Center, 300.0);
        assert_eq!(l.pill.w, 44.0);
        let l2 = layout(
            &scene(Some("hi"), None, true),
            &mut Fake,
            (920, 84),
            2.0,
            Align::Start,
            0.0,
        );
        assert_eq!(l2.pill.w, 244.0);
        assert_eq!(l2.icon.2, 24.0);
        assert_eq!(l2.text.expect("text").px, 30.0);
    }

    #[test]
    fn a_note_takes_at_most_its_share() {
        let note = "x".repeat(200);
        let l = layout(
            &scene(Some(&"y".repeat(200)), Some(&note), false),
            &mut Fake,
            (460, 42),
            1.0,
            Align::Center,
            0.0,
        );
        let room = 460.0 - (32.0 + 12.0 + 20.0);
        let n = l.note.expect("note");
        let t = l.text.expect("text");
        assert!(Fake.width(&n.text, n.px) <= room * NOTE_SHARE);
        assert!(t.text.ends_with('y') && n.text.starts_with('x'));
        assert!(n.x > t.x);
        // A note alone takes the whole room.
        let alone = layout(
            &scene(None, Some("Pasted"), false),
            &mut Fake,
            (460, 42),
            1.0,
            Align::Center,
            0.0,
        );
        assert_eq!(alone.note.expect("note").text, "Pasted");
    }
}
