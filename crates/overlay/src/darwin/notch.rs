//! The notch: a black shape that grows out of the camera housing (or the
//! top center of a screen without one), with a status icon and meter left
//! of the housing, a label right of it, and the text below.

use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSFontWeightMedium, NSMainMenuWindowLevel, NSPanel};
use objc2_quartz_core::{
    CAGradientLayer, CALayer, CAShapeLayer, CATextLayer, kCAAlignmentLeft, kCAAlignmentRight,
    kCALineCapRound, kCALineJoinRound, kCATruncationEnd,
};

use super::ca::{self, Attrs, Curve, Display};
use super::geometry::{
    self, BARS, Layout, MAX_LINES, Metrics, Notch as Housing, PathOp, Phase, Rect,
};
use super::helper::{self, Tick};
use crate::{Labels, Style, View};

const RED: (f64, f64, f64) = (1.0, 0.27, 0.23);
const ORANGE: (f64, f64, f64) = (1.0, 0.58, 0.0);
const GREY: (f64, f64, f64) = (0.57, 0.57, 0.57);
/// The notch's black, which its text layers fill behind their glyphs.
const BODY: (f64, f64, f64) = (0.0, 0.0, 0.0);
const FRAME: Duration = Duration::from_micros(16_667);
const TYPE: Duration = Duration::from_millis(30);

/// Which kind of view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Idle,
    Listening,
    Finishing,
    Done,
    Error,
}

impl Kind {
    fn of(view: &View) -> Kind {
        match view {
            View::Idle => Kind::Idle,
            View::Listening { .. } => Kind::Listening,
            View::Finishing { .. } => Kind::Finishing,
            View::Done { .. } => Kind::Done,
            View::Error { .. } => Kind::Error,
        }
    }

    fn recording(self) -> bool {
        matches!(self, Kind::Listening | Kind::Finishing)
    }
}

/// What the text area shows.
enum Content {
    None,
    Placeholder(String),
    Normal(String),
    Error(String, String),
}

/// How the text layer is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Normal,
    Placeholder,
    Error,
}

fn content(view: &View, style: &Style, labels: &Labels) -> Content {
    let live = |text: &str, placeholder: &str| {
        if !style.show_text {
            Content::None
        } else if !text.trim().is_empty() {
            Content::Normal(geometry::prepare(text))
        } else if !placeholder.trim().is_empty() {
            Content::Placeholder(placeholder.trim().to_owned())
        } else {
            Content::None
        }
    };
    match view {
        View::Idle => Content::None,
        View::Listening { text, .. } => live(text, &labels.listening),
        View::Finishing { text } => live(text, &labels.recognizing),
        View::Done { text, .. } if style.show_text && !text.trim().is_empty() => {
            Content::Normal(geometry::prepare(text))
        }
        View::Done { .. } => Content::None,
        View::Error { message, hint } => {
            Content::Error(geometry::prepare(message), geometry::prepare(hint))
        }
    }
}

pub(super) struct Notch {
    mtm: MainThreadMarker,
    scale: f64,
    panel: Retained<NSPanel>,
    body: Retained<CALayer>,
    mask: Retained<CAShapeLayer>,
    left: Retained<CALayer>,
    dot: Retained<CALayer>,
    check: Retained<CAShapeLayer>,
    bars: Vec<Retained<CALayer>>,
    right: Retained<CATextLayer>,
    text: Retained<CATextLayer>,
    hint: Retained<CATextLayer>,
    shimmer: Retained<CAGradientLayer>,
    text_attrs: Attrs,
    label_attrs: Attrs,
    line_h: f64,
    label_h: f64,

    display: Option<Display>,
    m: Metrics,
    layout: Layout,
    visible: bool,
    collapsing: bool,
    /// The current collapse's timer token.
    collapse: u64,
    phase: Phase,
    /// The shape last animated to: phase, text lines, right label width.
    target: Option<(Phase, u32, f64)>,
    /// Text lines of this session; grows, never shrinks, until it ends.
    lines: u32,
    kind: Kind,

    started: Option<Instant>,
    frozen: Option<Duration>,
    shown_secs: Option<u64>,
    right_text: String,
    right_alpha: f64,
    right_w: f64,
    check_drawn: bool,

    /// The meter's timer token; 0 when stopped.
    frame: u64,
    last_frame: Instant,
    level: f64,
    target_level: f64,
    wave: f64,

    /// The typewriter's timer token; 0 when stopped.
    typing: u64,
    /// The text the typewriter heads for, already cut to fit.
    typed: String,
    /// Characters of `typed` on screen.
    revealed: usize,
    text_kind: Option<TextKind>,
    text_shown: String,
    text_on: bool,
    shimmering: bool,
}

impl Notch {
    /// Builds the panel on the screen under the pointer, hidden, so the
    /// first session opens without delay.
    pub(super) fn new(mtm: MainThreadMarker, scale: f64) -> Notch {
        let s = scale;
        let (panel, root) = ca::panel(mtm, NSMainMenuWindowLevel + 2, false);

        let body = CALayer::new();
        body.setBackgroundColor(Some(&ca::rgb(BODY.0, BODY.1, BODY.2, 1.0)));
        let mask = CAShapeLayer::new();
        mask.setFillColor(Some(&ca::rgb(0.0, 0.0, 0.0, 1.0)));
        let mask_layer: &CALayer = &mask;
        // SAFETY: the mask is a fresh layer owned by this notch and in no
        // other tree.
        unsafe { body.setMask(Some(mask_layer)) };
        root.addSublayer(&body);

        let left = CALayer::new();
        body.addSublayer(&left);
        let dot = CALayer::new();
        left.addSublayer(&dot);
        let check = CAShapeLayer::new();
        check.setFillColor(None);
        check.setStrokeColor(Some(&ca::rgb(0.20, 0.84, 0.35, 1.0)));
        // SAFETY: Core Animation's constant names, valid for the process.
        unsafe {
            check.setLineCap(kCALineCapRound);
            check.setLineJoin(kCALineJoinRound);
        }
        check.setHidden(true);
        left.addSublayer(&check);
        let bars: Vec<Retained<CALayer>> = (0..BARS)
            .map(|_| {
                let bar = CALayer::new();
                bar.setBackgroundColor(Some(&ca::rgb(1.0, 1.0, 1.0, 0.92)));
                left.addSublayer(&bar);
                bar
            })
            .collect();

        let text_font = NSFont::systemFontOfSize(13.0 * s);
        // SAFETY: AppKit's constant weight, valid for the process.
        let medium = unsafe { NSFontWeightMedium };
        let label_font = NSFont::monospacedDigitSystemFontOfSize_weight(12.0 * s, medium);
        let text_attrs = ca::attrs(&text_font);
        let label_attrs = ca::attrs(&label_font);
        let line_h = ca::text_size(&text_attrs, "测Ag").height.ceil();
        let label_h = ca::text_size(&label_attrs, "00:00").height.ceil();

        let right = ca::text_layer(&label_font, BODY);
        // SAFETY: as above.
        unsafe {
            right.setAlignmentMode(kCAAlignmentRight);
            right.setTruncationMode(kCATruncationEnd);
        }
        body.addSublayer(&right);
        let text = ca::text_layer(&text_font, BODY);
        let hint = ca::text_layer(&text_font, BODY);
        for layer in [&text, &hint] {
            // SAFETY: as above.
            unsafe {
                layer.setAlignmentMode(kCAAlignmentLeft);
                layer.setTruncationMode(kCATruncationEnd);
            }
            layer.setOpacity(0.0);
            body.addSublayer(layer);
        }
        hint.setForegroundColor(Some(&ca::rgb(0.96, 0.96, 0.96, 0.6)));

        let housing = Housing {
            w: 0.0,
            h: 0.0,
            cx: 0.0,
        };
        let m = Metrics::new(housing, s, line_h);
        let layout = m.layout(Rect::new(0.0, 0.0, 0.0, 0.0));
        let mut notch = Notch {
            mtm,
            scale: s,
            panel,
            body,
            mask,
            left,
            dot,
            check,
            bars,
            right,
            text,
            hint,
            shimmer: ca::shimmer_mask(),
            text_attrs,
            label_attrs,
            line_h,
            label_h,
            display: None,
            m,
            layout,
            visible: false,
            collapsing: false,
            collapse: 0,
            phase: Phase::Collapsed,
            target: None,
            lines: 0,
            kind: Kind::Idle,
            started: None,
            frozen: None,
            shown_secs: None,
            right_text: String::new(),
            right_alpha: 0.0,
            right_w: 0.0,
            check_drawn: false,
            frame: 0,
            last_frame: Instant::now(),
            level: 0.0,
            target_level: 0.0,
            wave: 0.0,
            typing: 0,
            typed: String::new(),
            revealed: 0,
            text_kind: None,
            text_shown: String::new(),
            text_on: false,
            shimmering: false,
        };
        if let Some(d) = ca::display_under_mouse(mtm) {
            ca::quiet(|| notch.attach(d));
        }
        notch
    }

    pub(super) fn show(&mut self, view: &View, style: &Style, labels: &Labels) {
        ca::quiet(|| self.show_inner(view, style, labels));
    }

    pub(super) fn tick(&mut self, token: u64, tick: Tick) {
        ca::quiet(|| match tick {
            Tick::Frame if token == self.frame => self.on_frame(),
            Tick::Type if token == self.typing => self.on_type(),
            Tick::Collapse(step) if token == self.collapse && self.collapsing => {
                self.collapse_step(step);
            }
            _ => {}
        });
    }

    /// Takes the panel off screen at once.
    pub(super) fn hide(&mut self) {
        self.panel.orderOut(None);
        self.visible = false;
        self.collapsing = false;
        self.reset_session();
        self.kind = Kind::Idle;
    }

    fn show_inner(&mut self, view: &View, style: &Style, labels: &Labels) {
        let kind = Kind::of(view);
        if kind == Kind::Idle {
            if style.idle_visible {
                self.idle_marker(labels);
            } else {
                self.collapse();
            }
            return;
        }
        let fresh =
            !self.visible || self.collapsing || (kind.recording() && !self.kind.recording());
        if fresh {
            self.reset_session();
        }
        self.present(fresh);
        if !self.visible {
            return;
        }
        let entering = self.kind != kind;
        self.kind = kind;
        self.render_ears(view);
        self.render_text(content(view, style, labels), kind);
        let phase = if self.lines > 0 {
            Phase::Text
        } else {
            Phase::Ears
        };
        self.apply_shape(phase, Some(Curve::Spring));
        if entering && kind == Kind::Error {
            ca::shake(&self.body, 8.0 * self.scale);
        }
    }

    /// Puts the panel on screen: on the screen under the pointer for a new
    /// session, else where it is while that screen stays attached.
    fn present(&mut self, fresh: bool) {
        let collapsing = std::mem::take(&mut self.collapsing);
        let pinned = self.display.filter(|d| ca::connected(self.mtm, d));
        let want = if fresh || !self.visible || pinned.is_none() {
            ca::display_under_mouse(self.mtm)
        } else {
            pinned
        };
        let Some(d) = want else { return };
        if self.visible && self.display != Some(d) {
            self.panel.orderOut(None);
            self.visible = false;
        }
        self.attach(d);
        if !self.visible {
            self.apply_shape(Phase::Collapsed, None);
            for layer in [&*self.left, &self.right, &self.text, &self.hint] {
                ca::set_opacity(layer, 0.0);
            }
            self.text_on = false;
            self.panel.orderFrontRegardless();
            self.visible = true;
            ca::animate_opacity(&self.left, 1.0, Curve::EaseOut(0.2));
            ca::animate_opacity(&self.right, 1.0, Curve::EaseOut(0.2));
        } else if collapsing {
            ca::animate_opacity(&self.left, 1.0, Curve::EaseOut(0.2));
            ca::animate_opacity(&self.right, 1.0, Curve::EaseOut(0.2));
        }
    }

    /// Fits the panel and its layers to `d`; nothing when they already are.
    fn attach(&mut self, d: Display) {
        if self.display == Some(d) {
            return;
        }
        self.display = Some(d);
        self.m = Metrics::new(geometry::notch(&d.screen), self.scale, self.line_h);
        self.layout = self.m.layout(d.screen.frame);
        let m = self.m;
        let panel = self.layout.panel;
        self.panel.setFrame_display(ca::cg_rect(panel), false);
        let bounds = ca::cg_rect(Rect::new(0.0, 0.0, panel.w, panel.h));
        self.body.setFrame(bounds);
        self.mask.setFrame(bounds);
        let layers: [&CALayer; 9] = [
            &self.body,
            &self.mask,
            &self.left,
            &self.dot,
            &self.check,
            &self.right,
            &self.text,
            &self.hint,
            &self.shimmer,
        ];
        for layer in layers.into_iter().chain(self.bars.iter().map(|b| &**b)) {
            layer.setContentsScale(d.backing);
        }
        self.left
            .setBounds(ca::cg_rect(Rect::new(0.0, 0.0, m.left_ear_w, m.ears_h)));
        self.dot
            .setBounds(ca::cg_rect(Rect::new(0.0, 0.0, m.dot_d, m.dot_d)));
        self.dot.setCornerRadius(m.dot_d / 2.0);
        self.dot
            .setPosition(ca::point(m.icon_d / 2.0, m.ears_h / 2.0));
        let i = m.icon_d;
        self.check
            .setFrame(ca::cg_rect(Rect::new(0.0, (m.ears_h - i) / 2.0, i, i)));
        let check = ca::path(&[
            PathOp::Move(i * 0.10, i * 0.52),
            PathOp::Line(i * 0.40, i * 0.20),
            PathOp::Line(i * 0.92, i * 0.84),
        ]);
        self.check.setPath(Some(&check));
        self.check.setLineWidth(2.0 * m.s);
        for (n, bar) in self.bars.iter().enumerate() {
            bar.setCornerRadius(m.bar_w / 2.0);
            bar.setPosition(ca::point(m.bar_x(n), m.ears_h / 2.0));
        }
        self.layout_bars();
        self.right_w = self.label_w(&self.right_text.clone());
        self.target = None;
        let phase = self.phase;
        self.apply_shape(phase, None);
    }

    /// Morphs to `phase`, unless that shape is already the target.
    fn apply_shape(&mut self, phase: Phase, curve: Option<Curve>) {
        let lines = if phase == Phase::Text {
            self.lines.max(1)
        } else {
            0
        };
        self.phase = phase;
        let target = (phase, lines, self.right_w);
        if self.target == Some(target) {
            return;
        }
        self.target = Some(target);
        let m = self.m;
        let (cx, top) = (self.layout.cx, self.layout.top);
        let extra = if phase == Phase::Ears {
            m.ears_extra(self.right_w)
        } else {
            0.0
        };
        let shape = m.shape(phase, lines, extra);
        ca::animate_path(
            &self.mask,
            &ca::path(&geometry::notch_path(cx, top, &shape)),
            curve,
        );
        let mid = m.ears_mid_y(top);
        ca::animate_position(&self.left, ca::point(m.left_ear_x(cx, &shape), mid), curve);
        self.right
            .setBounds(ca::cg_rect(Rect::new(0.0, 0.0, self.right_w, self.label_h)));
        ca::animate_position(
            &self.right,
            ca::point(m.right_label_x(cx, &shape, self.right_w), mid),
            curve,
        );
        if lines > 0 {
            let r = m.text_rect(cx, top, lines);
            self.text.setFrame(ca::cg_rect(r));
            self.shimmer
                .setFrame(ca::cg_rect(Rect::new(0.0, 0.0, r.w, r.h)));
            self.hint
                .setFrame(ca::cg_rect(Rect::new(r.x, r.y, r.w, self.line_h)));
        }
    }

    fn render_ears(&mut self, view: &View) {
        match view {
            View::Listening { level, .. } => {
                self.set_dot(Some(RED));
                self.set_bars(1.0);
                if self.started.is_none() {
                    self.started = Some(Instant::now());
                }
                self.frozen = None;
                self.target_level = f64::from(*level);
                self.start_frames();
                self.set_clock();
            }
            View::Finishing { .. } => {
                self.set_dot(Some(ORANGE));
                self.set_bars(0.45);
                self.freeze();
                self.set_clock();
            }
            View::Done { label, .. } => {
                self.set_dot(None);
                self.set_bars(0.45);
                self.freeze();
                self.set_right(label.trim(), 1.0);
            }
            View::Error { .. } => {
                self.set_dot(Some(RED));
                self.set_bars(0.45);
                self.freeze();
                if self.started.is_some() {
                    self.set_clock();
                } else {
                    self.set_right("", 0.9);
                }
            }
            View::Idle => {}
        }
    }

    /// Stops the meter and the clock where they are.
    fn freeze(&mut self) {
        self.frame = 0;
        if self.frozen.is_none() {
            self.frozen = self.started.map(|s| s.elapsed());
        }
    }

    /// A coloured dot, or the green check for `None`.
    fn set_dot(&mut self, color: Option<(f64, f64, f64)>) {
        match color {
            Some((r, g, b)) => {
                self.dot.setHidden(false);
                self.dot.setBackgroundColor(Some(&ca::rgb(r, g, b, 1.0)));
                self.check.setHidden(true);
                self.check_drawn = false;
            }
            None => {
                self.dot.setHidden(true);
                self.check.setHidden(false);
                if !self.check_drawn {
                    self.check_drawn = true;
                    ca::draw_stroke(&self.check, 0.25);
                }
            }
        }
    }

    fn set_bars(&self, opacity: f32) {
        for bar in &self.bars {
            bar.setOpacity(opacity);
        }
    }

    fn layout_bars(&self) {
        let m = self.m;
        for (i, bar) in self.bars.iter().enumerate() {
            let h = geometry::bar_height(&m, i, self.level, self.wave);
            bar.setBounds(ca::cg_rect(Rect::new(0.0, 0.0, m.bar_w, h)));
        }
    }

    /// Width the right label takes: at least its ear, at most what the
    /// text body leaves beside the housing.
    fn label_w(&self, text: &str) -> f64 {
        let m = self.m;
        let room = ((m.text_body_w - m.notch.w) / 2.0 - m.ear_pad - 4.0 * m.s).max(m.right_ear_w);
        ca::text_size(&self.label_attrs, text)
            .width
            .ceil()
            .clamp(m.right_ear_w, room)
    }

    fn set_right(&mut self, text: &str, alpha: f64) {
        if self.right_text == text && self.right_alpha == alpha {
            return;
        }
        self.right_text = text.to_owned();
        self.right_alpha = alpha;
        self.right_w = self.label_w(text);
        ca::set_string(&self.right, text);
        self.right
            .setForegroundColor(Some(&ca::rgb(1.0, 1.0, 1.0, alpha)));
    }

    fn set_clock(&mut self) {
        let secs = self
            .frozen
            .or_else(|| self.started.map(|s| s.elapsed()))
            .map(|d| d.as_secs());
        self.shown_secs = secs;
        let (text, alpha) = match secs {
            Some(s) => (geometry::clock(s), 0.9),
            None => (geometry::clock(0), 0.45),
        };
        self.set_right(&text, alpha);
    }

    fn start_frames(&mut self) {
        if self.frame != 0 {
            return;
        }
        self.frame = helper::token();
        self.last_frame = Instant::now();
        helper::schedule(FRAME, self.frame, Tick::Frame);
    }

    fn on_frame(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;
        self.level = geometry::smooth(self.level, self.target_level, dt);
        self.wave += dt.min(0.1);
        self.layout_bars();
        if self.started.map(|s| s.elapsed().as_secs()) != self.shown_secs {
            self.set_clock();
        }
        helper::schedule(FRAME, self.frame, Tick::Frame);
    }

    /// Lines `text` wraps to in the text area.
    fn lines_of(&self, text: &str) -> u32 {
        let h = ca::text_height(&self.text_attrs, text, self.m.text_w());
        (h / self.line_h).round().max(1.0) as u32
    }

    /// The newest part of `text` that fits the text area.
    fn fit(&self, text: &str) -> String {
        geometry::fit_tail(text, |t| self.lines_of(t) <= MAX_LINES)
    }

    fn render_text(&mut self, content: Content, kind: Kind) {
        let fade = self.text_on && !self.text_shown.is_empty();
        match content {
            Content::None => {
                self.typing = 0;
                self.set_shimmer(false);
                if self.text_on {
                    ca::animate_opacity(&self.text, 0.0, Curve::EaseIn(0.12));
                    ca::animate_opacity(&self.hint, 0.0, Curve::EaseIn(0.12));
                    self.text_on = false;
                }
                self.lines = 0;
                self.typed.clear();
                self.revealed = 0;
                self.text_kind = None;
                self.text_shown.clear();
                return;
            }
            Content::Placeholder(placeholder) => {
                self.typing = 0;
                self.style_text(TextKind::Placeholder);
                self.revealed = placeholder.chars().count();
                self.typed = placeholder;
                self.swap_text(&self.typed.clone(), fade);
                ca::set_opacity(&self.hint, 0.0);
                self.lines = self.lines.max(1);
            }
            Content::Normal(text) => {
                let fitted = self.fit(&text);
                let before = self.text_kind;
                self.style_text(TextKind::Normal);
                self.lines = self.lines.max(self.lines_of(&fitted).min(MAX_LINES));
                ca::set_opacity(&self.hint, 0.0);
                let on_screen: String = self.typed.chars().take(self.revealed).collect();
                let append = kind == Kind::Listening
                    && matches!(before, None | Some(TextKind::Normal))
                    && fitted.starts_with(&on_screen);
                self.typed = fitted;
                if append {
                    if self.revealed < self.typed.chars().count() {
                        self.start_typing();
                    }
                } else {
                    self.typing = 0;
                    self.revealed = self.typed.chars().count();
                    self.swap_text(&self.typed.clone(), fade);
                }
            }
            Content::Error(message, hint) => {
                self.typing = 0;
                self.style_text(TextKind::Error);
                self.revealed = message.chars().count();
                self.typed = message;
                self.swap_text(&self.typed.clone(), false);
                if hint.is_empty() {
                    ca::set_opacity(&self.hint, 0.0);
                    self.lines = self.lines.max(1);
                } else {
                    ca::set_string(&self.hint, &hint);
                    ca::animate_opacity(&self.hint, 1.0, Curve::EaseOut(0.15));
                    self.lines = MAX_LINES;
                }
            }
        }
        self.set_shimmer(kind == Kind::Finishing);
        if !self.text_on {
            self.text_on = true;
            ca::animate_opacity(&self.text, 1.0, Curve::EaseOut(0.15));
        }
    }

    fn style_text(&mut self, kind: TextKind) {
        if self.text_kind == Some(kind) {
            return;
        }
        self.text_kind = Some(kind);
        self.text.setWrapped(kind == TextKind::Normal);
        let color = match kind {
            TextKind::Normal => ca::rgb(0.96, 0.96, 0.96, 1.0),
            TextKind::Placeholder => ca::rgb(0.96, 0.96, 0.96, 0.45),
            TextKind::Error => ca::rgb(1.0, 0.62, 0.58, 1.0),
        };
        self.text.setForegroundColor(Some(&color));
    }

    /// Replaces the text at once, cross-fading when `fade`.
    fn swap_text(&mut self, text: &str, fade: bool) {
        if self.text_shown == text {
            return;
        }
        if fade {
            ca::cross_fade(&self.text, 0.1);
        }
        self.set_text(text);
    }

    fn set_text(&mut self, text: &str) {
        ca::set_string(&self.text, text);
        text.clone_into(&mut self.text_shown);
    }

    fn start_typing(&mut self) {
        if self.typing == 0 {
            self.typing = helper::token();
            helper::schedule(TYPE, self.typing, Tick::Type);
        }
    }

    fn on_type(&mut self) {
        let total = self.typed.chars().count();
        let backlog = total.saturating_sub(self.revealed);
        if backlog == 0 {
            self.typing = 0;
            return;
        }
        let want = self.revealed + geometry::type_step(backlog);
        let byte = self
            .typed
            .char_indices()
            .nth(want)
            .map_or(self.typed.len(), |(i, _)| i);
        let byte = geometry::boundary(&self.typed, byte);
        let shown = self.typed[..byte].to_owned();
        self.revealed = shown.chars().count();
        self.set_text(&shown);
        if self.revealed < total {
            helper::schedule(TYPE, self.typing, Tick::Type);
        } else {
            self.typing = 0;
        }
    }

    fn set_shimmer(&mut self, on: bool) {
        if on == self.shimmering {
            return;
        }
        self.shimmering = on;
        let key = objc2_foundation::NSString::from_str("sweep");
        if on {
            self.shimmer.setFrame(ca::cg_rect(Rect::new(
                0.0,
                0.0,
                self.text.bounds().size.width,
                self.text.bounds().size.height,
            )));
            let mask: &CALayer = &self.shimmer;
            // SAFETY: the gradient is this notch's own layer, masking only
            // the text layer.
            unsafe { self.text.setMask(Some(mask)) };
            ca::sweep(&self.shimmer);
        } else {
            self.shimmer.removeAnimationForKey(&key);
            // SAFETY: removing a mask is always valid.
            unsafe { self.text.setMask(None) };
        }
    }

    fn stop_activity(&mut self) {
        self.frame = 0;
        self.typing = 0;
        self.set_shimmer(false);
    }

    /// Forgets the session: clock, text, meter.
    fn reset_session(&mut self) {
        self.stop_activity();
        self.started = None;
        self.frozen = None;
        self.shown_secs = None;
        self.lines = 0;
        self.typed.clear();
        self.revealed = 0;
        self.text_kind = None;
        self.text_shown.clear();
        self.level = 0.0;
        self.target_level = 0.0;
        self.wave = 0.0;
        self.check_drawn = false;
        self.layout_bars();
        if self.text_on {
            ca::animate_opacity(&self.text, 0.0, Curve::EaseIn(0.12));
            ca::animate_opacity(&self.hint, 0.0, Curve::EaseIn(0.12));
            self.text_on = false;
        }
    }

    /// Text out, then the ears, then the panel.
    fn collapse(&mut self) {
        if !self.visible || self.collapsing {
            self.kind = Kind::Idle;
            return;
        }
        self.stop_activity();
        self.kind = Kind::Idle;
        self.collapsing = true;
        self.collapse = helper::token();
        if self.phase == Phase::Text {
            ca::animate_opacity(&self.text, 0.0, Curve::EaseIn(0.12));
            ca::animate_opacity(&self.hint, 0.0, Curve::EaseIn(0.12));
            self.text_on = false;
            self.apply_shape(Phase::Ears, Some(Curve::EaseIn(0.18)));
            helper::schedule(Duration::from_millis(180), self.collapse, Tick::Collapse(1));
        } else {
            self.collapse_step(1);
        }
    }

    fn collapse_step(&mut self, step: u8) {
        if step == 1 {
            ca::animate_opacity(&self.left, 0.0, Curve::EaseIn(0.12));
            ca::animate_opacity(&self.right, 0.0, Curve::EaseIn(0.12));
            self.apply_shape(Phase::Collapsed, Some(Curve::EaseIn(0.2)));
            helper::schedule(Duration::from_millis(220), self.collapse, Tick::Collapse(2));
        } else {
            self.panel.orderOut(None);
            self.visible = false;
            self.collapsing = false;
            self.reset_session();
        }
    }

    /// The small idle marker: a grey dot and the idle label in the ears.
    fn idle_marker(&mut self, labels: &Labels) {
        if !self.visible || self.collapsing || self.kind != Kind::Idle {
            let new_screen = !self.visible || self.collapsing;
            self.reset_session();
            self.kind = Kind::Idle;
            self.present(new_screen);
            if !self.visible {
                return;
            }
        }
        self.set_dot(Some(GREY));
        self.set_bars(0.0);
        self.set_right(labels.idle.trim(), 0.45);
        self.apply_shape(Phase::Ears, Some(Curve::Spring));
    }
}

impl Drop for Notch {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}
