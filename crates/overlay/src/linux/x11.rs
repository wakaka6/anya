//! The overlay on X11: an override-redirect window the window manager never
//! sees, with an empty input shape so clicks pass through. With a
//! compositing manager it uses a 32-bit visual and draws with alpha;
//! without one the window is shaped to the capsule and drawn opaque. Each
//! session it moves to the monitor under the pointer.

use std::borrow::Cow;
use std::os::fd::AsFd;

use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::ReplyOrIdError;
use x11rb::image::{BitsPerPixel, ColorComponent, Image, ImageOrder, PixelLayout, ScanlinePad};
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ClipOrdering, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, EventMask, PropMode, Rectangle, StackMode, VisualClass, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::layout::{Rect, anchor, margin, monitor_at, place, surface_size};
use super::model::{BACKGROUND, Scene};
use super::paint::{Canvas, Painter, flatten, pill_spans};
use super::{Host, Window};
use crate::{OverlayError, Style};

fn error(e: impl std::fmt::Display) -> OverlayError {
    OverlayError::Backend(e.to_string())
}

/// An X11 connection and the visual the overlay draws with.
pub(super) struct X11 {
    conn: RustConnection,
    root: u32,
    root_size: (u16, u16),
    visual: u32,
    depth: u8,
    /// A 32-bit visual under a compositing manager: per-pixel alpha.
    argb: bool,
    /// The visual's pixel layout when it has no alpha.
    layout: PixelLayout,
    window_type: u32,
    notification: u32,
    win: Option<Win>,
    pixels: Vec<u8>,
    dirty: bool,
    alive: bool,
}

/// The overlay window.
struct Win {
    id: u32,
    gc: u32,
    colormap: Option<u32>,
    size: (u32, u32),
    mapped: bool,
    /// The session it was placed for.
    session: u64,
    /// The capsule its shape was cut to, without alpha.
    shaped: Option<(i32, i32)>,
}

/// The layout of the pixels the painter writes: 0xAARRGGBB.
fn rgb888() -> PixelLayout {
    let c =
        |shift| ColorComponent::new(8, shift).unwrap_or_else(|_| unreachable!("8-bit component"));
    PixelLayout::new(c(16), c(8), c(0))
}

impl X11 {
    /// Connects to `DISPLAY` and picks the visual.
    pub(super) fn open() -> Result<X11, OverlayError> {
        if !super::display_set() {
            return Err(error("DISPLAY is not set"));
        }
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| error(format!("cannot open the X display: {e}")))?;
        let has_shape = conn
            .extension_information(shape::X11_EXTENSION_NAME)
            .map_err(error)?
            .is_some();
        if !has_shape {
            return Err(error("the X server lacks the SHAPE extension"));
        }
        let atom = |name: &str| -> Result<u32, OverlayError> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(error)?
                .reply()
                .map_err(error)?
                .atom)
        };
        let cm = atom(&format!("_NET_WM_CM_S{screen}"))?;
        let window_type = atom("_NET_WM_WINDOW_TYPE")?;
        let notification = atom("_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
        let composited = conn
            .get_selection_owner(cm)
            .map_err(error)?
            .reply()
            .map_err(error)?
            .owner
            != x11rb::NONE;

        let s = &conn.setup().roots[screen];
        let argb_visual = s
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .flat_map(|d| &d.visuals)
            .find(|v| {
                v.class == VisualClass::TRUE_COLOR
                    && (v.red_mask, v.green_mask, v.blue_mask) == (0xff0000, 0xff00, 0xff)
            })
            .map(|v| v.visual_id)
            .filter(|_| composited);
        let (visual, depth, layout) = match argb_visual {
            Some(visual) => (visual, 32, rgb888()),
            None => {
                let root_visual = s
                    .allowed_depths
                    .iter()
                    .filter(|d| d.depth == s.root_depth)
                    .flat_map(|d| &d.visuals)
                    .find(|v| v.visual_id == s.root_visual)
                    .ok_or_else(|| error("the X root visual is not listed"))?;
                let layout = PixelLayout::from_visual_type(*root_visual)
                    .map_err(|e| error(format!("the X root visual is not true colour: {e}")))?;
                (s.root_visual, s.root_depth, layout)
            }
        };
        let (root, root_size) = (s.root, (s.width_in_pixels, s.height_in_pixels));
        Ok(X11 {
            conn,
            root,
            root_size,
            visual,
            depth,
            argb: argb_visual.is_some(),
            layout,
            window_type,
            notification,
            win: None,
            pixels: Vec::new(),
            dirty: false,
            alive: true,
        })
    }

    /// Reads every pending event: an expose asks for a redraw.
    fn drain(&mut self) {
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::Expose(e))) if e.count == 0 => self.dirty = true,
                Ok(Some(Event::Error(e))) => tracing::debug!(?e, "X11 error for the overlay"),
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("the overlay's X11 connection failed: {e}");
                    self.alive = false;
                    break;
                }
            }
        }
    }

    fn create(&self, style: &Style) -> Result<Win, ReplyOrIdError> {
        let (w, h) = surface_size(style.scale);
        let size = (w.min(u32::from(u16::MAX)), h.min(u32::from(u16::MAX)));
        let id = self.conn.generate_id()?;
        let colormap = if self.argb {
            let colormap = self.conn.generate_id()?;
            self.conn
                .create_colormap(ColormapAlloc::NONE, colormap, self.root, self.visual)?;
            Some(colormap)
        } else {
            None
        };
        let mut aux = CreateWindowAux::new()
            .override_redirect(1)
            .save_under(1)
            .background_pixel(0)
            .border_pixel(0)
            .event_mask(EventMask::EXPOSURE);
        if let Some(colormap) = colormap {
            aux = aux.colormap(colormap);
        }
        self.conn.create_window(
            self.depth,
            id,
            self.root,
            0,
            0,
            size.0 as u16,
            size.1 as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            self.visual,
            &aux,
        )?;
        // No input region: clicks go to the window below.
        self.conn.shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            ClipOrdering::UNSORTED,
            id,
            0,
            0,
            &[],
        )?;
        if !self.argb {
            // Nothing shows until the first draw cuts the capsule.
            self.conn.shape_rectangles(
                shape::SO::SET,
                shape::SK::BOUNDING,
                ClipOrdering::UNSORTED,
                id,
                0,
                0,
                &[],
            )?;
        }
        // Compositors give notifications their shadow and stacking rules.
        self.conn.change_property32(
            PropMode::REPLACE,
            id,
            self.window_type,
            AtomEnum::ATOM,
            &[self.notification],
        )?;
        self.conn.change_property8(
            PropMode::REPLACE,
            id,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"anya-overlay\0Anya\0",
        )?;
        let gc = self.conn.generate_id()?;
        self.conn.create_gc(gc, id, &CreateGCAux::new())?;
        Ok(Win {
            id,
            gc,
            colormap,
            size,
            mapped: false,
            session: 0,
            shaped: None,
        })
    }

    /// The monitor under the pointer; the whole screen without RandR 1.5.
    fn monitor(&self) -> Result<Rect, ReplyOrIdError> {
        let pointer = self.conn.query_pointer(self.root)?.reply()?;
        let monitors: Vec<Rect> = self
            .conn
            .randr_get_monitors(self.root, true)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| {
                reply
                    .monitors
                    .iter()
                    .map(|m| Rect {
                        x: m.x.into(),
                        y: m.y.into(),
                        w: m.width.into(),
                        h: m.height.into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let screen = Rect {
            x: 0,
            y: 0,
            w: self.root_size.0.into(),
            h: self.root_size.1.into(),
        };
        Ok(monitor_at(&monitors, pointer.root_x.into(), pointer.root_y.into()).unwrap_or(screen))
    }

    fn show(
        &mut self,
        win: &mut Win,
        scene: &Scene,
        style: &Style,
        painter: &mut Painter,
    ) -> Result<(), ReplyOrIdError> {
        if !win.mapped || win.session != scene.session {
            let monitor = self.monitor()?;
            let (x, y) = place(
                anchor(style.position),
                monitor,
                win.size,
                margin(style.scale),
            );
            let aux = ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .stack_mode(StackMode::ABOVE);
            self.conn.configure_window(win.id, &aux)?;
            if !win.mapped {
                self.conn.map_window(win.id)?;
                win.mapped = true;
            }
            win.session = scene.session;
        }

        let (w, h) = win.size;
        self.pixels.resize(w as usize * h as usize * 4, 0);
        let mut canvas = Canvas::new(&mut self.pixels, w, h);
        let l = painter.paint(
            scene,
            &mut canvas,
            style.scale as f32,
            anchor(style.position).align,
        );
        if !self.argb {
            flatten(&mut self.pixels, BACKGROUND);
            let pill = (l.pill.x as i32, l.pill.w as i32);
            if win.shaped != Some(pill) {
                let rects: Vec<Rectangle> = pill_spans(l.pill)
                    .into_iter()
                    .map(|s| Rectangle {
                        x: s.x as i16,
                        y: s.y as i16,
                        width: s.w as u16,
                        height: s.h as u16,
                    })
                    .collect();
                self.conn.shape_rectangles(
                    shape::SO::SET,
                    shape::SK::BOUNDING,
                    ClipOrdering::UNSORTED,
                    win.id,
                    0,
                    0,
                    &rects,
                )?;
                win.shaped = Some(pill);
            }
        }

        let (w, h) = (w as u16, h as u16);
        let depth = if self.argb { 32 } else { 24 };
        let image = Image::new(
            w,
            h,
            ScanlinePad::Pad32,
            depth,
            BitsPerPixel::B32,
            ImageOrder::LsbFirst,
            Cow::Borrowed(&self.pixels),
        )?;
        let image = if self.argb {
            image.native(self.conn.setup())?
        } else {
            image.reencode(rgb888(), self.layout, self.conn.setup())?
        };
        image.put(&self.conn, win.id, win.gc, 0, 0)?;
        self.conn.flush()?;
        Ok(())
    }

    fn hide(&mut self) {
        if let Some(win) = self.win.as_mut()
            && win.mapped
        {
            win.mapped = false;
            if self.conn.unmap_window(win.id).is_err() || self.conn.flush().is_err() {
                self.alive = false;
            }
        }
    }

    fn destroy(&mut self) {
        if let Some(win) = self.win.take() {
            let _ = self.conn.free_gc(win.gc);
            let _ = self.conn.destroy_window(win.id);
            if let Some(colormap) = win.colormap {
                let _ = self.conn.free_colormap(colormap);
            }
            let _ = self.conn.flush();
        }
    }
}

impl Window for X11 {
    fn watch(&mut self, handle: &LoopHandle<'static, Host<X11>>) -> Result<(), OverlayError> {
        let fd = self
            .conn
            .stream()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| error(format!("cannot watch the X11 connection: {e}")))?;
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        handle
            .insert_source(source, |_, _, host: &mut Host<X11>| {
                host.win.drain();
                Ok(PostAction::Continue)
            })
            .map_err(|e| error(format!("cannot watch the X11 connection: {}", e.error)))?;
        Ok(())
    }

    fn take_dirty(&mut self) -> bool {
        // Replies read while placing the window may have queued events the
        // socket no longer signals.
        self.drain();
        std::mem::take(&mut self.dirty)
    }

    fn present(&mut self, scene: Option<&Scene>, style: &Style, painter: &mut Painter) {
        let Some(scene) = scene else {
            self.hide();
            return;
        };
        let mut win = match self.win.take() {
            Some(win) => win,
            None => match self.create(style) {
                Ok(win) => win,
                Err(e) => {
                    tracing::warn!("cannot create the overlay window: {e}");
                    self.alive = false;
                    return;
                }
            },
        };
        if let Err(e) = self.show(&mut win, scene, style, painter) {
            tracing::warn!("cannot draw the overlay window: {e}");
            self.alive = false;
        }
        self.win = Some(win);
    }

    fn rebuild(&mut self) {
        self.destroy();
    }

    fn alive(&self) -> bool {
        self.alive
    }

    fn close(&mut self) {
        self.destroy();
    }
}

/// Whether an X display accepts a connection.
pub(super) fn probe() -> bool {
    x11rb::connect(None).is_ok()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anya_core::desktop::Desktop;
    use x11rb::protocol::xproto::{ImageFormat, MapState};

    use super::*;
    use crate::linux::Backend;
    use crate::{Labels, Position, View};

    /// The mapped override-redirect window of class `anya-overlay`.
    fn overlay(conn: &RustConnection, root: u32) -> Option<(u32, MapState)> {
        let tree = conn.query_tree(root).ok()?.reply().ok()?;
        tree.children.into_iter().find_map(|w| {
            let class = conn
                .get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 64)
                .ok()?
                .reply()
                .ok()?;
            if !class.value.starts_with(b"anya-overlay\0") {
                return None;
            }
            let attrs = conn.get_window_attributes(w).ok()?.reply().ok()?;
            assert!(
                attrs.override_redirect,
                "the window manager would manage it"
            );
            Some((w, attrs.map_state))
        })
    }

    // Needs an X server. Under Xvfb, with two monitors side by side:
    //   Xvfb :99 -screen 0 1280x800x24 & export DISPLAY=:99
    //   xrandr --setmonitor L 640/170x800/250+0+0 none
    //   xrandr --setmonitor R 640/170x800/250+640+0 none
    //   cargo test -p anya-overlay -- --ignored x11_overlay
    #[test]
    #[ignore = "needs an X display"]
    fn x11_overlay_draws_on_the_monitor_under_the_pointer_and_hides() {
        let (conn, screen) = x11rb::connect(None).expect("an X display");
        let s = &conn.setup().roots[screen];
        let (root, rw, rh) = (
            s.root,
            i32::from(s.width_in_pixels),
            i32::from(s.height_in_pixels),
        );
        let (px, py) = (rw * 3 / 4, rh / 2);
        conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, px as i16, py as i16)
            .expect("warp");
        conn.flush().expect("flush");
        let monitors: Vec<Rect> = conn
            .randr_get_monitors(root, true)
            .expect("randr")
            .reply()
            .map(|r| {
                r.monitors
                    .iter()
                    .map(|m| Rect {
                        x: m.x.into(),
                        y: m.y.into(),
                        w: m.width.into(),
                        h: m.height.into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let screen_rect = Rect {
            x: 0,
            y: 0,
            w: rw,
            h: rh,
        };
        let monitor = monitor_at(&monitors, px, py).unwrap_or(screen_rect);

        let style = Style {
            position: Position::BottomCenter,
            scale: 1.0,
            idle_visible: false,
            show_text: true,
        };
        let labels = Labels {
            listening: "Listening…".into(),
            recognizing: "Recognizing…".into(),
            idle: "Idle".into(),
        };
        let backend = Backend::start(Desktop::X11, style, labels).expect("overlay");
        backend.show(View::Listening {
            level: 0.8,
            text: "Hello, 测试 world".into(),
        });
        std::thread::sleep(Duration::from_millis(500));

        let (win, state) = overlay(&conn, root).expect("an overlay window");
        assert_eq!(state, MapState::VIEWABLE);
        let g = conn
            .get_geometry(win)
            .expect("geometry")
            .reply()
            .expect("geometry");
        assert_eq!((g.width, g.height), (460, 42));
        // 32 under a compositing manager, the root depth without one.
        eprintln!(
            "overlay depth {} on {monitor:?} of {} monitors",
            g.depth,
            monitors.len()
        );
        let at = place(anchor(Position::BottomCenter), monitor, (460, 42), 28);
        assert_eq!((i32::from(g.x), i32::from(g.y)), at, "on {monitor:?}");

        let img = conn
            .get_image(ImageFormat::Z_PIXMAP, win, 0, 0, 460, 42, !0)
            .expect("image")
            .reply()
            .expect("image");
        let px: Vec<&[u8]> = img.data.chunks_exact(4).collect();
        // The capsule's fill, and bright text, meter and dot on it.
        let fill = px
            .iter()
            .filter(|p| p[0] < 30 && p[1] < 30 && p[2] < 30)
            .count();
        let bright = px.iter().filter(|p| p[2] > 200).count();
        assert!(fill > 460 * 42 / 4, "fill {fill}");
        assert!(bright > 100, "bright {bright}");

        backend.show(View::Idle);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(overlay(&conn, root).map(|o| o.1), Some(MapState::UNMAPPED));
        drop(backend);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            overlay(&conn, root),
            None,
            "the window outlived the overlay"
        );
    }
}
