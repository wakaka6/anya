//! AppKit and Core Animation plumbing the notch and the capsule share:
//! panels, layers, animations from what is on screen, text measuring,
//! screens and main-queue timers.

use std::ptr;
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    AllocAnyThread, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSFont, NSFontAttributeName, NSPanel, NSScreen,
    NSStringDrawing, NSStringDrawingOptions, NSStringNSExtendedStringDrawing, NSView,
    NSWindowAnimationBehavior, NSWindowCollectionBehavior, NSWindowLevel, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGContext, CGMutablePath, CGPath};
use objc2_foundation::{
    NSArray, NSAttributedStringKey, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString,
    NSValue,
};
use objc2_quartz_core::{
    CABasicAnimation, CAGradientLayer, CALayer, CAMediaTiming, CAMediaTimingFunction, CAShapeLayer,
    CASpringAnimation, CATextLayer, CATransaction, CATransition, kCAMediaTimingFunctionEaseIn,
    kCAMediaTimingFunctionEaseOut, kCATransitionFade,
};

use super::geometry::{self, PathOp, Rect, Screen};

pub(super) fn rgb(r: f64, g: f64, b: f64, a: f64) -> CFRetained<CGColor> {
    CGColor::new_generic_rgb(r, g, b, a)
}

pub(super) fn point(x: f64, y: f64) -> CGPoint {
    CGPoint::new(x, y)
}

pub(super) fn cg_rect(r: Rect) -> CGRect {
    CGRect::new(CGPoint::new(r.x, r.y), CGSize::new(r.w, r.h))
}

fn rect(r: CGRect) -> Rect {
    Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
}

/// A Core Foundation object as the Objective-C object it bridges to.
fn cf_object(cf: &CFType) -> &AnyObject {
    cf.as_ref()
}

/// Runs `f` with implicit layer animations off: every change the overlay
/// animates, it animates explicitly.
pub(super) fn quiet<R>(f: impl FnOnce() -> R) -> R {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    let out = f();
    CATransaction::commit();
    out
}

/// How an explicit animation moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Curve {
    /// The expanding spring: mass 1, stiffness 320, damping 26.
    Spring,
    /// Ease in over the seconds given.
    EaseIn(f64),
    /// Ease out over the seconds given.
    EaseOut(f64),
}

fn animation(
    key: &NSString,
    from: &AnyObject,
    to: &AnyObject,
    curve: Curve,
) -> Retained<CABasicAnimation> {
    let anim = match curve {
        Curve::Spring => {
            let spring = CASpringAnimation::animationWithKeyPath(Some(key));
            spring.setMass(1.0);
            spring.setStiffness(320.0);
            spring.setDamping(26.0);
            spring.setDuration(spring.settlingDuration());
            Retained::into_super(spring)
        }
        Curve::EaseIn(secs) | Curve::EaseOut(secs) => {
            let anim = CABasicAnimation::animationWithKeyPath(Some(key));
            anim.setDuration(secs);
            // SAFETY: Core Animation's constant names, valid for the process.
            let name = unsafe {
                if matches!(curve, Curve::EaseIn(_)) {
                    kCAMediaTimingFunctionEaseIn
                } else {
                    kCAMediaTimingFunctionEaseOut
                }
            };
            anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(name)));
            anim
        }
    };
    // SAFETY: `from` and `to` are values of the animated key's type: the
    // callers pass NSNumber for opacity and stroke, NSValue for position and
    // CGPath for path.
    unsafe {
        anim.setFromValue(Some(from));
        anim.setToValue(Some(to));
    }
    anim
}

/// What is on screen now: the presentation layer while an animation runs.
fn on_screen(layer: &CALayer) -> Retained<CALayer> {
    // SAFETY: the presentation layer is a read-only copy; it is only read.
    unsafe { layer.presentationLayer() }.unwrap_or_else(|| layer.retain())
}

/// Sets `layer`'s opacity, animating from what is on screen.
pub(super) fn animate_opacity(layer: &CALayer, to: f32, curve: Curve) {
    let key = NSString::from_str("opacity");
    let from = on_screen(layer).opacity();
    quiet(|| layer.setOpacity(to));
    if from == to {
        layer.removeAnimationForKey(&key);
    } else {
        let anim = animation(
            &key,
            &NSNumber::new_f32(from),
            &NSNumber::new_f32(to),
            curve,
        );
        layer.addAnimation_forKey(&anim, Some(&key));
    }
}

/// Sets `layer`'s opacity at once, ending any fade.
pub(super) fn set_opacity(layer: &CALayer, to: f32) {
    layer.removeAnimationForKey(&NSString::from_str("opacity"));
    quiet(|| layer.setOpacity(to));
}

/// Moves `layer`, animating from what is on screen when `curve` is given.
pub(super) fn animate_position(layer: &CALayer, to: CGPoint, curve: Option<Curve>) {
    let key = NSString::from_str("position");
    let from = on_screen(layer).position();
    quiet(|| layer.setPosition(to));
    match curve {
        Some(curve) if from != to => {
            let anim = animation(&key, &NSValue::new(from), &NSValue::new(to), curve);
            layer.addAnimation_forKey(&anim, Some(&key));
        }
        _ => layer.removeAnimationForKey(&key),
    }
}

/// Reshapes `layer`, morphing from what is on screen when `curve` is given.
pub(super) fn animate_path(layer: &CAShapeLayer, to: &CGPath, curve: Option<Curve>) {
    let key = NSString::from_str("path");
    // SAFETY: as in `on_screen`.
    let from = unsafe { layer.presentationLayer() }
        .and_then(|p| p.downcast::<CAShapeLayer>().ok())
        .and_then(|p| p.path())
        .or_else(|| layer.path());
    quiet(|| layer.setPath(Some(to)));
    match (curve, from) {
        (Some(curve), Some(from)) => {
            let anim = animation(&key, cf_object(&from), cf_object(to), curve);
            layer.addAnimation_forKey(&anim, Some(&key));
        }
        _ => layer.removeAnimationForKey(&key),
    }
}

/// Draws a shape layer's stroke from nothing.
pub(super) fn draw_stroke(layer: &CAShapeLayer, secs: f64) {
    let key = NSString::from_str("strokeEnd");
    quiet(|| layer.setStrokeEnd(1.0));
    let anim = animation(
        &key,
        &NSNumber::new_f64(0.0),
        &NSNumber::new_f64(1.0),
        Curve::EaseOut(secs),
    );
    layer.addAnimation_forKey(&anim, Some(&key));
}

/// Cross-fades whatever `layer` shows next.
pub(super) fn cross_fade(layer: &CALayer, secs: f64) {
    let fade = CATransition::animation();
    // SAFETY: Core Animation's constant name, valid for the process.
    fade.setType(unsafe { kCATransitionFade });
    fade.setDuration(secs);
    // SAFETY: as above.
    let ease = unsafe { kCAMediaTimingFunctionEaseOut };
    fade.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(ease)));
    layer.addAnimation_forKey(&fade, Some(&NSString::from_str("swap")));
}

/// A repeating horizontal shake of `layer`, `amplitude` points wide.
pub(super) fn shake(layer: &CALayer, amplitude: f64) {
    let anim = objc2_quartz_core::CAKeyframeAnimation::animationWithKeyPath(Some(
        &NSString::from_str("transform.translation.x"),
    ));
    let a = amplitude;
    let values = objects(&[0.0, -a, 0.85 * a, -0.6 * a, 0.35 * a, -0.15 * a, 0.0]);
    // SAFETY: the values are NSNumbers, what a translation key animates.
    unsafe { anim.setValues(Some(&values)) };
    anim.setKeyTimes(Some(&numbers(&[0.0, 0.15, 0.3, 0.45, 0.6, 0.8, 1.0])));
    anim.setAdditive(true);
    anim.setDuration(0.4);
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("shake")));
}

pub(super) fn numbers(values: &[f64]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = values.iter().map(|&v| NSNumber::new_f64(v)).collect();
    NSArray::from_retained_slice(&numbers)
}

fn objects(values: &[f64]) -> Retained<NSArray<AnyObject>> {
    let numbers: Vec<Retained<NSNumber>> = values.iter().map(|&v| NSNumber::new_f64(v)).collect();
    let objects: Vec<&AnyObject> = numbers.iter().map(|n| -> &AnyObject { n }).collect();
    NSArray::from_slice(&objects)
}

/// Colours as the array a gradient layer takes.
fn colors(colors: &[CFRetained<CGColor>]) -> Retained<NSArray<AnyObject>> {
    let objects: Vec<&AnyObject> = colors.iter().map(|c| cf_object(c)).collect();
    NSArray::from_slice(&objects)
}

/// A mask that dims text to 45 % except for a bright band.
pub(super) fn shimmer_mask() -> Retained<CAGradientLayer> {
    let layer = CAGradientLayer::new();
    let c = colors(&[
        rgb(1.0, 1.0, 1.0, 0.45),
        rgb(1.0, 1.0, 1.0, 1.0),
        rgb(1.0, 1.0, 1.0, 0.45),
    ]);
    // SAFETY: a gradient's colours are CGColors.
    unsafe { layer.setColors(Some(&c)) };
    layer.setStartPoint(point(0.0, 0.5));
    layer.setEndPoint(point(1.0, 0.5));
    layer.setLocations(Some(&numbers(&[0.0, 0.5, 1.0])));
    layer
}

/// Sweeps the bright band across `layer` every 1.3 s, until removed.
pub(super) fn sweep(layer: &CAGradientLayer) {
    let key = NSString::from_str("locations");
    let from = numbers(&[-0.3, -0.15, 0.0]);
    let to = numbers(&[1.0, 1.15, 1.3]);
    let (from, to): (&AnyObject, &AnyObject) = (&from, &to);
    let anim = CABasicAnimation::animationWithKeyPath(Some(&key));
    // SAFETY: `locations` animates arrays of NSNumber.
    unsafe {
        anim.setFromValue(Some(from));
        anim.setToValue(Some(to));
    }
    anim.setDuration(1.3);
    anim.setRepeatCount(f32::INFINITY);
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("sweep")));
}

/// A path from geometry's steps.
pub(super) fn path(ops: &[PathOp]) -> CFRetained<CGMutablePath> {
    let p = CGMutablePath::new();
    for op in ops {
        // SAFETY: a null transform means none; the path is ours and alive.
        unsafe {
            match *op {
                PathOp::Move(x, y) => CGMutablePath::move_to_point(Some(&p), ptr::null(), x, y),
                PathOp::Line(x, y) => CGMutablePath::add_line_to_point(Some(&p), ptr::null(), x, y),
                PathOp::Curve(ax, ay, bx, by, x, y) => {
                    CGMutablePath::add_curve_to_point(Some(&p), ptr::null(), ax, ay, bx, by, x, y)
                }
                PathOp::Close => CGMutablePath::close_subpath(Some(&p)),
            }
        }
    }
    p
}

/// Font attributes for measuring text.
pub(super) type Attrs = Retained<NSDictionary<NSAttributedStringKey, AnyObject>>;

pub(super) fn attrs(font: &NSFont) -> Attrs {
    // SAFETY: AppKit's constant key, valid for the process.
    let key: &NSAttributedStringKey = unsafe { NSFontAttributeName };
    let font: &AnyObject = font;
    NSDictionary::from_slices(&[key], &[font])
}

/// `text` on one line.
pub(super) fn text_size(attrs: &Attrs, text: &str) -> CGSize {
    // SAFETY: `attrs` maps the font key to an NSFont.
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(attrs)) }
}

/// Height of `text` wrapped at `width`.
pub(super) fn text_height(attrs: &Attrs, text: &str, width: f64) -> f64 {
    // SAFETY: as in `text_size`; no drawing context is needed to measure.
    let r = unsafe {
        NSString::from_str(text).boundingRectWithSize_options_attributes_context(
            CGSize::new(width, f64::MAX),
            NSStringDrawingOptions::UsesLineFragmentOrigin,
            Some(attrs),
            None,
        )
    };
    r.size.height
}

define_class!(
    /// A text layer whose bitmap is opaque: it paints its background colour
    /// before the glyphs, since Core Animation does not clear an opaque
    /// layer's bitmap between draws.
    // SAFETY: CATextLayer may be subclassed; the one override calls the
    // superclass and draws only into the context it is handed.
    #[unsafe(super(CATextLayer, CALayer, NSObject))]
    #[name = "AnyaOpaqueTextLayer"]
    struct OpaqueTextLayer;

    impl OpaqueTextLayer {
        #[unsafe(method(drawInContext:))]
        fn draw_in_context(&self, ctx: &CGContext) {
            CGContext::set_fill_color_with_color(Some(ctx), self.backgroundColor().as_deref());
            CGContext::fill_rect(Some(ctx), self.bounds());
            CGContext::set_allows_font_smoothing(Some(ctx), true);
            CGContext::set_should_smooth_fonts(Some(ctx), true);
            // SAFETY: CATextLayer's own drawInContext:, with the context
            // Core Animation passed in.
            let () = unsafe { msg_send![super(self), drawInContext: ctx] };
        }
    }
);

/// A text layer on `backdrop`, the colour behind it. Core Text smooths
/// glyphs only in an opaque bitmap; on a clear layer they draw thinner and
/// fainter than AppKit's text.
pub(super) fn text_layer(font: &NSFont, backdrop: (f64, f64, f64)) -> Retained<CATextLayer> {
    // SAFETY: `init` is NSObject's designated initializer, which
    // OpaqueTextLayer inherits unchanged.
    let layer: Retained<OpaqueTextLayer> = unsafe { msg_send![OpaqueTextLayer::alloc(), init] };
    let layer = Retained::into_super(layer);
    // SAFETY: NSFont is toll-free bridged with CTFont, one of the types
    // `font` takes.
    let cf: &CFType = unsafe { &*ptr::from_ref(font).cast::<CFType>() };
    // SAFETY: as above.
    unsafe { layer.setFont(Some(cf)) };
    layer.setFontSize(font.pointSize());
    layer.setBackgroundColor(Some(&rgb(backdrop.0, backdrop.1, backdrop.2, 1.0)));
    layer.setOpaque(true);
    layer
}

pub(super) fn set_string(layer: &CATextLayer, text: &str) {
    let text = NSString::from_str(text);
    // SAFETY: `string` takes an NSString.
    unsafe { layer.setString(Some(&text)) };
}

/// A borderless, non-activating, click-through panel on every space, and
/// the layer it hosts.
pub(super) fn panel(
    mtm: MainThreadMarker,
    level: NSWindowLevel,
    shadow: bool,
) -> (Retained<NSPanel>, Retained<CALayer>) {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(10.0, 10.0));
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the panel is kept in a `Retained` and never released by close.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel(level);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setIgnoresMouseEvents(true);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(shadow);
    panel.setHidesOnDeactivate(false);
    panel.setCanHide(false);
    panel.setAnimationBehavior(NSWindowAnimationBehavior::None);

    let root = CALayer::new();
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
    view.setLayer(Some(&root));
    view.setWantsLayer(true);
    panel.setContentView(Some(&view));
    (panel, root)
}

/// A screen as the overlay sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Display {
    pub(super) screen: Screen,
    pub(super) backing: f64,
}

fn describe(screen: &NSScreen) -> Display {
    let frame = rect(screen.frame());
    let (safe_top, aux_left, aux_right) = if screen.respondsToSelector(sel!(safeAreaInsets)) {
        (
            screen.safeAreaInsets().top,
            screen.auxiliaryTopLeftArea().size.width,
            screen.auxiliaryTopRightArea().size.width,
        )
    } else {
        (0.0, 0.0, 0.0)
    };
    Display {
        screen: Screen {
            frame,
            visible: rect(screen.visibleFrame()),
            safe_top,
            aux_left,
            aux_right,
        },
        backing: screen.backingScaleFactor(),
    }
}

/// The screen under the mouse pointer; the main screen, then the first one,
/// when the pointer is on none.
pub(super) fn display_under_mouse(mtm: MainThreadMarker) -> Option<Display> {
    let screens: Vec<Retained<NSScreen>> = NSScreen::screens(mtm).iter().collect();
    let frames: Vec<Rect> = screens.iter().map(|s| rect(s.frame())).collect();
    let mouse = NSEvent::mouseLocation();
    geometry::screen_under(&frames, mouse.x, mouse.y)
        .and_then(|i| screens.get(i).cloned())
        .or_else(|| NSScreen::mainScreen(mtm))
        .or_else(|| screens.first().cloned())
        .map(|s| describe(&s))
}

/// Whether a screen like `display` is still attached, unchanged.
pub(super) fn connected(mtm: MainThreadMarker, display: &Display) -> bool {
    NSScreen::screens(mtm)
        .iter()
        .any(|s| describe(&s) == *display)
}

/// Runs `f` on the main queue after `delay`.
pub(super) fn after(delay: Duration, f: impl FnOnce() + Send + 'static) {
    let when = DispatchTime::try_from(delay).unwrap_or(DispatchTime::NOW);
    // `after` only fails for times it cannot represent, which a short delay is not.
    let _ = DispatchQueue::main().after(when, f);
}
