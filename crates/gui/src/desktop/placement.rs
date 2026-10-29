//! Where the tray panel opens: beside the tray icon, on the side away from
//! the screen edge the icon sits on, kept inside that screen.

/// A rectangle in one coordinate space (logical points of one screen),
/// origin top-left, y growing down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.width
    }

    fn bottom(&self) -> f64 {
        self.y + self.height
    }

    fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub(crate) fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// The screen edge a tray icon sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// The edge of `screen` nearest the middle of `anchor`; a top bar wins a tie.
pub(crate) fn nearest_edge(anchor: Rect, screen: Rect) -> Edge {
    let (cx, cy) = anchor.center();
    let mut best = (Edge::Top, cy - screen.y);
    for (edge, distance) in [
        (Edge::Bottom, screen.bottom() - cy),
        (Edge::Left, cx - screen.x),
        (Edge::Right, screen.right() - cx),
    ] {
        if distance < best.1 {
            best = (edge, distance);
        }
    }
    best.0
}

/// The top-left corner for a panel of `size` opened from `anchor` (the tray
/// icon, or the pointer where the icon's place is unknown): `gap` away from
/// it, centred on it along the bar, and at least `gap` inside `screen`.
pub(crate) fn place(anchor: Rect, size: (f64, f64), screen: Rect, gap: f64) -> (f64, f64) {
    let (width, height) = size;
    let (cx, cy) = anchor.center();
    let (x, y) = match nearest_edge(anchor, screen) {
        Edge::Top => (cx - width / 2.0, anchor.bottom() + gap),
        Edge::Bottom => (cx - width / 2.0, anchor.y - gap - height),
        Edge::Left => (anchor.right() + gap, cy - height / 2.0),
        Edge::Right => (anchor.x - gap - width, cy - height / 2.0),
    };
    (
        clamp(x, width, screen.x, screen.right(), gap),
        clamp(y, height, screen.y, screen.bottom(), gap),
    )
}

/// `start` moved so `[start, start + len]` lies `margin` inside `[lo, hi]`;
/// a span longer than that starts at `lo`.
fn clamp(start: f64, len: f64, lo: f64, hi: f64, margin: f64) -> f64 {
    let min = lo + margin;
    let max = hi - margin - len;
    if max < min { lo } else { start.clamp(min, max) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: (f64, f64) = (340.0, 596.0);
    const GAP: f64 = 6.0;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn screen() -> Rect {
        rect(0.0, 0.0, 1440.0, 900.0)
    }

    #[test]
    fn opens_below_a_menu_bar_icon_centred_on_it() {
        let icon = rect(1000.0, 0.0, 24.0, 24.0);
        assert_eq!(nearest_edge(icon, screen()), Edge::Top);
        assert_eq!(place(icon, PANEL, screen(), GAP), (842.0, 30.0));
    }

    #[test]
    fn stays_inside_the_right_edge_for_an_icon_in_the_corner() {
        let icon = rect(1410.0, 0.0, 24.0, 24.0);
        let (x, y) = place(icon, PANEL, screen(), GAP);
        assert_eq!(x, 1440.0 - 340.0 - GAP);
        assert_eq!(y, 30.0);
    }

    #[test]
    fn stays_inside_the_left_edge_for_an_icon_in_the_corner() {
        let icon = rect(4.0, 0.0, 24.0, 24.0);
        assert_eq!(place(icon, PANEL, screen(), GAP), (GAP, 30.0));
    }

    #[test]
    fn opens_above_a_bottom_taskbar_icon() {
        let icon = rect(700.0, 868.0, 32.0, 32.0);
        assert_eq!(nearest_edge(icon, screen()), Edge::Bottom);
        assert_eq!(
            place(icon, PANEL, screen(), GAP),
            (546.0, 868.0 - GAP - 596.0)
        );
    }

    #[test]
    fn opens_beside_a_side_panel_icon() {
        let left = rect(0.0, 400.0, 32.0, 32.0);
        assert_eq!(nearest_edge(left, screen()), Edge::Left);
        assert_eq!(place(left, PANEL, screen(), GAP), (38.0, 118.0));

        let right = rect(1408.0, 400.0, 32.0, 32.0);
        assert_eq!(nearest_edge(right, screen()), Edge::Right);
        assert_eq!(
            place(right, PANEL, screen(), GAP),
            (1408.0 - GAP - 340.0, 118.0)
        );
    }

    #[test]
    fn side_panel_icon_near_a_corner_keeps_the_panel_on_screen() {
        let icon = rect(1408.0, 860.0, 32.0, 32.0);
        let (_, y) = place(icon, PANEL, screen(), GAP);
        assert_eq!(y, 900.0 - 596.0 - GAP);
    }

    #[test]
    fn works_on_a_screen_that_is_not_at_the_origin() {
        let second = rect(1440.0, -200.0, 1920.0, 1080.0);
        let icon = rect(3300.0, -200.0, 24.0, 24.0);
        let (x, y) = place(icon, PANEL, second, GAP);
        assert_eq!(x, 1440.0 + 1920.0 - 340.0 - GAP);
        assert_eq!(y, -200.0 + 24.0 + GAP);
    }

    #[test]
    fn a_pointer_anchor_has_no_size() {
        let pointer = rect(1200.0, 12.0, 0.0, 0.0);
        assert_eq!(place(pointer, PANEL, screen(), GAP), (1030.0, 18.0));
    }

    #[test]
    fn a_screen_shorter_than_the_panel_starts_it_at_the_top() {
        let small = rect(0.0, 0.0, 1024.0, 560.0);
        let icon = rect(900.0, 0.0, 24.0, 24.0);
        let (_, y) = place(icon, PANEL, small, GAP);
        assert_eq!(y, 0.0);
    }

    #[test]
    fn contains_is_half_open() {
        let r = rect(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains((0.0, 9.9)));
        assert!(!r.contains((10.0, 5.0)));
    }
}
