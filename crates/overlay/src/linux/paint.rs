//! Software drawing into a premultiplied ARGB8888 buffer (bytes B, G, R, A),
//! the format both wl_shm and a 32-bit X11 visual take. Edges are
//! antialiased analytically: a pixel's coverage is how far its center lies
//! inside the shape's edge, so curves and straight sides blend alike and the
//! compositor (KWin included) gets no fringe from straight alpha.

use super::font::Fonts;
use super::layout::{self, Align, Area, BAR_MAX, BAR_MIN, BAR_WIDTH, Layout};
use super::model::{BACKGROUND, BARS, Icon, Rgba, Scene};

/// Pixels to draw into, 4 bytes each, rows packed.
pub(super) struct Canvas<'a> {
    data: &'a mut [u8],
    w: u32,
    h: u32,
}

impl<'a> Canvas<'a> {
    /// `data` holds `w * h` pixels; extra bytes are left alone, a short
    /// buffer is drawn as far as it goes.
    pub(super) fn new(data: &'a mut [u8], w: u32, h: u32) -> Canvas<'a> {
        Canvas { data, w, h }
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// Fully transparent.
    pub(super) fn clear(&mut self) {
        self.data.fill(0);
    }

    /// Draws `color` over the pixel at `(x, y)`, `coverage` of it.
    pub(super) fn blend(&mut self, x: i32, y: i32, color: Rgba, coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        let a = f32::from(color.a) / 255.0 * coverage.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        let Some(px) = self.data.get_mut(i..i + 4) else {
            return;
        };
        let inv = 1.0 - a;
        let mix = |src: u8, dst: u8| (f32::from(src) * a + f32::from(dst) * inv).round() as u8;
        px[0] = mix(color.b, px[0]);
        px[1] = mix(color.g, px[1]);
        px[2] = mix(color.r, px[2]);
        px[3] = mix(255, px[3]);
    }

    /// Fills the pixels within `(x0, y0, x1, y1)` by the signed distance
    /// `sdf` gives at each pixel center.
    fn fill(&mut self, bounds: (f32, f32, f32, f32), color: Rgba, sdf: impl Fn(f32, f32) -> f32) {
        let (x0, y0) = (
            bounds.0.floor().max(0.0) as i32,
            bounds.1.floor().max(0.0) as i32,
        );
        let (x1, y1) = (
            bounds.2.ceil().min(self.w as f32) as i32,
            bounds.3.ceil().min(self.h as f32) as i32,
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let c = coverage(sdf(x as f32 + 0.5, y as f32 + 0.5));
                self.blend(x, y, color, c);
            }
        }
    }

    pub(super) fn rounded_rect(&mut self, area: Area, radius: f32, color: Rgba) {
        let bounds = (
            area.x - 1.0,
            area.y - 1.0,
            area.x + area.w + 1.0,
            area.y + area.h + 1.0,
        );
        self.fill(bounds, color, |x, y| {
            rounded_rect_distance(x, y, area, radius)
        });
    }

    pub(super) fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgba) {
        let bounds = (cx - r - 1.0, cy - r - 1.0, cx + r + 1.0, cy + r + 1.0);
        self.fill(bounds, color, |x, y| (x - cx).hypot(y - cy) - r);
    }

    /// A stroke `width` wide through `points`, with round caps and joins.
    pub(super) fn polyline(&mut self, points: &[(f32, f32)], width: f32, color: Rgba) {
        let half = width / 2.0;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in points {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let bounds = (
            x0 - half - 1.0,
            y0 - half - 1.0,
            x1 + half + 1.0,
            y1 + half + 1.0,
        );
        self.fill(bounds, color, |x, y| {
            points
                .windows(2)
                .map(|s| segment_distance(x, y, s[0], s[1]))
                .fold(f32::MAX, f32::min)
                - half
        });
    }
}

/// How much of a pixel a shape covers, from the signed distance of the
/// pixel's center to the shape's edge (negative inside): a one-pixel ramp.
pub(super) fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

/// The signed distance from `(x, y)` to a rectangle with corners rounded by
/// `radius`.
pub(super) fn rounded_rect_distance(x: f32, y: f32, area: Area, radius: f32) -> f32 {
    let (hw, hh) = (area.w / 2.0, area.h / 2.0);
    let r = radius.clamp(0.0, hw.min(hh));
    let qx = (x - area.x - hw).abs() - (hw - r);
    let qy = (y - area.y - hh).abs() - (hh - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// The distance from `(x, y)` to the segment from `a` to `b`.
fn segment_distance(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len > 0.0 {
        (((x - a.0) * dx + (y - a.1) * dy) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (x - a.0 - t * dx).hypot(y - a.1 - t * dy)
}

/// One run of a window shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) w: u32,
    pub(super) h: u32,
}

/// The pixels at least half inside the capsule `pill`, as rectangles: equal
/// rows merge. A window without alpha takes this as its shape.
pub(super) fn pill_spans(pill: Area) -> Vec<Span> {
    let radius = pill.h / 2.0;
    let (x0, x1) = (pill.x.floor() as i32, (pill.x + pill.w).ceil() as i32);
    let (y0, y1) = (pill.y.floor() as i32, (pill.y + pill.h).ceil() as i32);
    let mut spans: Vec<Span> = Vec::new();
    for y in y0..y1 {
        let inside =
            |x: i32| rounded_rect_distance(x as f32 + 0.5, y as f32 + 0.5, pill, radius) <= 0.0;
        let Some(left) = (x0..x1).find(|&x| inside(x)) else {
            continue;
        };
        let right = (left..x1).rev().find(|&x| inside(x)).unwrap_or(left);
        let w = (right - left + 1) as u32;
        match spans.last_mut() {
            Some(last) if last.x == left && last.w == w && last.y + last.h as i32 == y => {
                last.h += 1;
            }
            _ => spans.push(Span {
                x: left,
                y,
                w,
                h: 1,
            }),
        }
    }
    spans
}

/// Composites premultiplied `data` over an opaque `background`, for a
/// window without alpha.
pub(super) fn flatten(data: &mut [u8], background: Rgba) {
    for px in data.chunks_exact_mut(4) {
        let inv = 1.0 - f32::from(px[3]) / 255.0;
        let under =
            |src: u8, bg: u8| (f32::from(src) + f32::from(bg) * inv).round().min(255.0) as u8;
        px[0] = under(px[0], background.b);
        px[1] = under(px[1], background.g);
        px[2] = under(px[2], background.r);
        px[3] = 255;
    }
}

/// Draws scenes. Within one session the capsule only grows, so it does not
/// jitter while a transcript is revised.
pub(super) struct Painter {
    fonts: Fonts,
    session: u64,
    /// The widest capsule this session, in logical pixels.
    grown: f32,
}

impl Painter {
    pub(super) fn new(fonts: Fonts) -> Painter {
        Painter {
            fonts,
            session: 0,
            grown: 0.0,
        }
    }

    /// Draws `scene` over the whole canvas at `k` device pixels per logical
    /// pixel; returns the layout it drew.
    pub(super) fn paint(
        &mut self,
        scene: &Scene,
        canvas: &mut Canvas<'_>,
        k: f32,
        align: Align,
    ) -> Layout {
        if scene.session != self.session {
            self.session = scene.session;
            self.grown = 0.0;
        }
        let l = layout::layout(
            scene,
            &mut self.fonts,
            canvas.size(),
            k,
            align,
            self.grown * k,
        );
        if !scene.idle {
            self.grown = self.grown.max(l.pill.w / k);
        }
        canvas.clear();
        canvas.rounded_rect(l.pill, l.pill.h / 2.0, BACKGROUND);

        let (cx, cy, d) = l.icon;
        match scene.icon {
            Icon::Dot(color) => canvas.circle(cx, cy, d / 2.0, color),
            Icon::Check(color) => {
                let at = |u: f32, v: f32| (cx + (u - 0.5) * d, cy + (v - 0.5) * d);
                let points = [at(0.1, 0.55), at(0.4, 0.82), at(0.92, 0.2)];
                canvas.polyline(&points, d * 0.18, color);
            }
        }

        if let (Some(meter), Some(area)) = (scene.meter, l.meter) {
            let (bw, step) = (BAR_WIDTH * k, area.w / BARS as f32);
            for (i, f) in meter.bars.iter().enumerate() {
                let bh = ((BAR_MIN + (BAR_MAX - BAR_MIN) * f.clamp(0.0, 1.0)) * k).min(area.h);
                let bar = Area {
                    x: area.x + i as f32 * step + (step - bw) / 2.0,
                    y: area.y + (area.h - bh) / 2.0,
                    w: bw,
                    h: bh,
                };
                canvas.rounded_rect(bar, bw / 2.0, meter.color);
            }
        }

        let lines = [
            (&l.text, scene.text.as_ref().map(|t| t.color)),
            (&l.note, scene.note.as_ref().map(|t| t.color)),
        ];
        for (placed, color) in lines {
            if let (Some(p), Some(color)) = (placed, color) {
                let (ascent, descent) = self.fonts.metrics(p.px);
                let baseline = (l.pill.y + l.pill.h / 2.0 + (ascent + descent) / 2.0).round();
                self.fonts.draw(canvas, &p.text, p.x, baseline, p.px, color);
            }
        }
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba = Rgba {
        r: 255,
        g: 40,
        b: 30,
        a: 255,
    };

    fn area(x: f32, y: f32, w: f32, h: f32) -> Area {
        Area { x, y, w, h }
    }

    #[test]
    fn coverage_is_full_inside_empty_outside_and_half_on_the_edge() {
        let a = area(10.0, 10.5, 100.0, 39.0);
        let at = |x: f32, y: f32| coverage(rounded_rect_distance(x, y, a, 19.5));
        assert_eq!(at(60.5, 30.5), 1.0);
        assert_eq!(at(0.5, 0.5), 0.0);
        // The top edge runs through the middle of row 10.
        assert!((at(60.5, 10.5) - 0.5).abs() < 1e-4);
        // Row 11 is inside, row 9 outside.
        assert_eq!(at(60.5, 11.5), 1.0);
        assert_eq!(at(60.5, 9.5), 0.0);
    }

    #[test]
    fn rounded_corners_are_antialiased_and_symmetric() {
        let a = area(0.0, 0.0, 122.0, 42.0);
        let at = |x: i32, y: i32| {
            coverage(rounded_rect_distance(
                x as f32 + 0.5,
                y as f32 + 0.5,
                a,
                21.0,
            ))
        };
        // The corner pixel is outside, the curve has partial pixels.
        assert_eq!(at(0, 0), 0.0);
        let partial = (0..21)
            .filter(|&x| (0.01..0.99).contains(&at(x, 2)))
            .count();
        assert!(partial >= 1, "row 2 has no blended pixel");
        for y in 0..42 {
            for x in 0..61 {
                let c = at(x, y);
                assert!((c - at(121 - x, y)).abs() < 1e-4, "mirror x at {x},{y}");
                assert!((c - at(x, 41 - y)).abs() < 1e-4, "mirror y at {x},{y}");
            }
        }
        // A radius larger than the box is the box's half height.
        let pill = rounded_rect_distance(61.0, 0.5, a, 1000.0);
        assert!((pill - rounded_rect_distance(61.0, 0.5, a, 21.0)).abs() < 1e-4);
    }

    #[test]
    fn drawing_keeps_colours_premultiplied() {
        let mut data = vec![0u8; 60 * 30 * 4];
        let mut c = Canvas::new(&mut data, 60, 30);
        c.rounded_rect(area(0.5, 0.5, 59.0, 29.0), 14.5, BACKGROUND);
        c.circle(15.0, 15.0, 6.3, RED);
        c.polyline(
            &[(25.0, 15.0), (30.0, 20.0), (40.0, 8.0)],
            2.2,
            RED.alpha_for_test(140),
        );
        c.blend(-1, 3, RED, 1.0);
        c.blend(60, 3, RED, 1.0);
        for px in data.chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{px:?}");
        }
        // The dot's middle is the colour itself, opaque.
        let i = (15 * 60 + 15) * 4;
        assert_eq!(&data[i..i + 4], &[30, 40, 255, 255]);
        // The corner stays clear.
        assert_eq!(&data[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn a_window_without_alpha_is_shaped_like_the_capsule() {
        let spans = pill_spans(area(10.0, 0.0, 122.0, 42.0));
        assert_eq!(spans.iter().map(|s| s.h).sum::<u32>(), 42);
        for s in &spans {
            // Centered on the capsule: x - 10 on the left equals the gap on the right.
            assert_eq!(s.x - 10, 132 - (s.x + s.w as i32), "{s:?}");
        }
        let widest = spans.iter().map(|s| s.w).max();
        assert_eq!(widest, Some(122));
        assert!(spans[0].w < 122);
        assert_eq!(spans[0].y, 0);
        // The straight middle is one merged rectangle.
        assert!(spans.iter().any(|s| s.w == 122 && s.h > 1));
    }

    #[test]
    fn flattening_lays_the_capsule_on_its_own_colour() {
        let mut data = vec![0u8; 3 * 4];
        let mut c = Canvas::new(&mut data, 3, 1);
        c.blend(0, 0, BACKGROUND, 1.0);
        c.blend(1, 0, RED, 1.0);
        flatten(&mut data, BACKGROUND);
        // The translucent fill over the same colour reads as that colour.
        assert_eq!(&data[0..4], &[20, 20, 20, 255]);
        assert_eq!(&data[4..8], &[30, 40, 255, 255]);
        assert_eq!(&data[8..12], &[20, 20, 20, 255]);
    }

    impl Rgba {
        fn alpha_for_test(self, a: u8) -> Rgba {
            Rgba { a, ..self }
        }
    }
}
