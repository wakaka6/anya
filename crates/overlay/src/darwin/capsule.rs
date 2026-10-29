//! The capsule: a dark pill with a coloured dot, the state's words and,
//! when text is shown, the newest words recognized, at one of the screen's
//! corners or edges.

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSFloatingWindowLevel, NSFont, NSPanel};
use objc2_quartz_core::{CALayer, CATextLayer, kCATruncationEnd, kCATruncationStart};

use super::ca::{self, Attrs, Display};
use super::geometry::{self, CAPSULE_MARGIN, Rect};
use crate::{Labels, Style, View};

const RED: (f64, f64, f64) = (1.0, 0.27, 0.23);
const ORANGE: (f64, f64, f64) = (1.0, 0.58, 0.0);
const GREEN: (f64, f64, f64) = (0.20, 0.78, 0.35);
const GREY: (f64, f64, f64) = (0.57, 0.57, 0.57);

pub(super) struct Capsule {
    mtm: MainThreadMarker,
    scale: f64,
    panel: Retained<NSPanel>,
    pill: Retained<CALayer>,
    dot: Retained<CALayer>,
    label: Retained<CATextLayer>,
    detail: Retained<CATextLayer>,
    label_attrs: Attrs,
    detail_attrs: Attrs,
    label_h: f64,
    detail_h: f64,
    display: Option<Display>,
    visible: bool,
}

impl Capsule {
    pub(super) fn new(mtm: MainThreadMarker, scale: f64) -> Capsule {
        let s = scale;
        let (panel, root) = ca::panel(mtm, NSFloatingWindowLevel, true);
        let pill = CALayer::new();
        pill.setBackgroundColor(Some(&ca::rgb(0.08, 0.08, 0.08, 0.94)));
        root.addSublayer(&pill);
        let dot = CALayer::new();
        pill.addSublayer(&dot);
        let label_font = NSFont::boldSystemFontOfSize(13.0 * s);
        let detail_font = NSFont::systemFontOfSize(12.0 * s);
        let label = ca::text_layer(&label_font);
        let detail = ca::text_layer(&detail_font);
        detail.setForegroundColor(Some(&ca::rgb(0.96, 0.96, 0.96, 0.7)));
        pill.addSublayer(&label);
        pill.addSublayer(&detail);
        let label_attrs = ca::attrs(&label_font);
        let detail_attrs = ca::attrs(&detail_font);
        let label_h = ca::text_size(&label_attrs, "测Ag").height.ceil();
        let detail_h = ca::text_size(&detail_attrs, "测Ag").height.ceil();
        Capsule {
            mtm,
            scale: s,
            panel,
            pill,
            dot,
            label,
            detail,
            label_attrs,
            detail_attrs,
            label_h,
            detail_h,
            display: None,
            visible: false,
        }
    }

    pub(super) fn show(&mut self, view: &View, style: &Style, labels: &Labels) {
        ca::quiet(|| self.show_inner(view, style, labels));
    }

    pub(super) fn hide(&mut self) {
        self.panel.orderOut(None);
        self.visible = false;
    }

    fn show_inner(&mut self, view: &View, style: &Style, labels: &Labels) {
        let text = |t: &str| {
            if style.show_text {
                geometry::prepare(t)
            } else {
                String::new()
            }
        };
        let white = (0.96, 0.96, 0.96);
        // Dot, words, their colour, the detail, and whether the detail is
        // live text whose newest words matter most.
        let (dot, label, color, detail, live) = match view {
            View::Idle if !style.idle_visible => {
                self.hide();
                return;
            }
            View::Idle => (GREY, labels.idle.clone(), white, String::new(), false),
            View::Listening { text: t, .. } => {
                (RED, labels.listening.clone(), white, text(t), true)
            }
            View::Finishing { text: t } => {
                (ORANGE, labels.recognizing.clone(), white, text(t), true)
            }
            View::Done { text: t, label } => (GREEN, label.clone(), white, text(t), false),
            View::Error { message, hint } => (
                RED,
                geometry::prepare(message),
                (1.0, 0.62, 0.58),
                geometry::prepare(hint),
                false,
            ),
        };
        let label = label.trim().to_owned();

        let pinned = self.display.filter(|d| ca::connected(self.mtm, d));
        let display = if self.visible && pinned.is_some() {
            pinned
        } else {
            ca::display_under_mouse(self.mtm)
        };
        let Some(d) = display else { return };
        self.display = Some(d);

        let label_w = ca::text_size(&self.label_attrs, &label).width;
        let detail_w = if detail.is_empty() {
            0.0
        } else {
            ca::text_size(&self.detail_attrs, &detail).width
        };
        let l = geometry::capsule_layout(self.scale, label_w, detail_w);
        let (x, y) = geometry::capsule_origin(
            style.position,
            d.screen.visible,
            l.w,
            l.h,
            CAPSULE_MARGIN * self.scale,
        );
        self.panel
            .setFrame_display(ca::cg_rect(Rect::new(x, y, l.w, l.h)), false);
        for layer in [&*self.pill, &self.dot, &self.label, &self.detail] {
            layer.setContentsScale(d.backing);
        }
        self.pill
            .setFrame(ca::cg_rect(Rect::new(0.0, 0.0, l.w, l.h)));
        self.pill.setCornerRadius(l.h / 2.0);
        self.dot.setFrame(ca::cg_rect(Rect::new(
            l.dot_x,
            (l.h - l.dot) / 2.0,
            l.dot,
            l.dot,
        )));
        self.dot.setCornerRadius(l.dot / 2.0);
        self.dot
            .setBackgroundColor(Some(&ca::rgb(dot.0, dot.1, dot.2, 1.0)));

        self.label.setFrame(ca::cg_rect(Rect::new(
            l.label_x,
            (l.h - self.label_h) / 2.0,
            l.label_w,
            self.label_h,
        )));
        // SAFETY: Core Animation's constant name, valid for the process.
        unsafe { self.label.setTruncationMode(kCATruncationEnd) };
        self.label
            .setForegroundColor(Some(&ca::rgb(color.0, color.1, color.2, 1.0)));
        ca::set_string(&self.label, &label);

        self.detail.setHidden(l.detail_w == 0.0);
        self.detail.setFrame(ca::cg_rect(Rect::new(
            l.detail_x,
            (l.h - self.detail_h) / 2.0,
            l.detail_w,
            self.detail_h,
        )));
        // SAFETY: as above.
        unsafe {
            self.detail.setTruncationMode(if live {
                kCATruncationStart
            } else {
                kCATruncationEnd
            });
        }
        ca::set_string(&self.detail, &detail);

        if !self.visible {
            self.panel.orderFrontRegardless();
            self.visible = true;
        }
    }
}

impl Drop for Capsule {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}
