//! The overlay on Wayland: a wlr-layer-shell surface on the overlay layer,
//! which takes no keyboard focus and an empty input region, so it never
//! steals focus or a click. The compositor places it on the output it
//! chooses for a new surface (the focused one), so each session gets a new
//! surface.

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_output, wl_registry, wl_shm, wl_surface,
};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    registry_handlers,
};

use super::layout::{Align, anchor, margin, surface_size};
use super::model::Scene;
use super::paint::{Canvas, Painter};
use super::{Host, Window};
use crate::{OverlayError, Style};

/// The layer-shell namespace compositors match rules against.
const NAMESPACE: &str = "anya-overlay";

fn error(e: impl std::fmt::Display) -> OverlayError {
    OverlayError::Backend(e.to_string())
}

/// A Wayland connection with the globals the overlay binds.
pub(super) struct Wayland {
    conn: Connection,
    /// Until [`Window::watch`] hands it to the event loop.
    queue: Option<EventQueue<Host<Wayland>>>,
    qh: QueueHandle<Host<Wayland>>,
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    shm: Shm,
    layers: LayerShell,
    pool: SlotPool,
    surface: Option<Surface>,
    /// The session whose surface the compositor closed: it stays closed
    /// until the next session.
    closed: Option<u64>,
    dirty: bool,
    alive: bool,
}

/// One session's layer surface.
struct Surface {
    layer: LayerSurface,
    session: u64,
    /// Size in surface-local (logical) pixels.
    size: (u32, u32),
    configured: bool,
    /// The output's integer scale.
    scale: i32,
    /// The buffer on screen, kept until the next one replaces it.
    buffer: Option<Buffer>,
}

impl Wayland {
    /// Connects to `WAYLAND_DISPLAY` and binds the compositor, shared
    /// memory and layer-shell.
    pub(super) fn open() -> Result<Wayland, OverlayError> {
        let conn = Connection::connect_to_env()
            .map_err(|e| error(format!("cannot connect to the Wayland display: {e}")))?;
        let (globals, queue) = registry_queue_init::<Host<Wayland>>(&conn)
            .map_err(|e| error(format!("cannot read the Wayland globals: {e}")))?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|e| error(format!("the Wayland compositor global is missing: {e}")))?;
        let shm = Shm::bind(&globals, &qh)
            .map_err(|e| error(format!("the Wayland shm global is missing: {e}")))?;
        let layers = LayerShell::bind(&globals, &qh)
            .map_err(|_| error("the Wayland compositor does not support wlr-layer-shell"))?;
        let outputs = OutputState::new(&globals, &qh);
        let registry = RegistryState::new(&globals);
        let pool = SlotPool::new(4096, &shm)
            .map_err(|e| error(format!("cannot create Wayland shared memory: {e}")))?;
        Ok(Wayland {
            conn,
            queue: Some(queue),
            qh,
            registry,
            outputs,
            compositor,
            shm,
            layers,
            pool,
            surface: None,
            closed: None,
            dirty: false,
            alive: true,
        })
    }

    fn flush(&mut self) {
        if let Err(e) = self.conn.flush() {
            let blocked = matches!(&e, smithay_client_toolkit::reexports::client::backend::WaylandError::Io(io)
                if io.kind() == std::io::ErrorKind::WouldBlock);
            if !blocked {
                tracing::warn!("the overlay's Wayland connection failed: {e}");
                self.alive = false;
            }
        }
    }

    /// A new layer surface for `session`, sized and anchored for `style`.
    /// It draws once the compositor configures it.
    fn create(&self, session: u64, style: &Style) -> Surface {
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layers.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some(NAMESPACE),
            None,
        );
        let at = anchor(style.position);
        let mut edges = if at.top { Anchor::TOP } else { Anchor::BOTTOM };
        match at.align {
            Align::Start => edges |= Anchor::LEFT,
            Align::End => edges |= Anchor::RIGHT,
            Align::Center => {}
        }
        let m = margin(style.scale);
        let on = |edge| if edges.contains(edge) { m } else { 0 };
        let size = surface_size(style.scale);
        layer.set_anchor(edges);
        layer.set_margin(
            on(Anchor::TOP),
            on(Anchor::RIGHT),
            on(Anchor::BOTTOM),
            on(Anchor::LEFT),
        );
        layer.set_size(size.0, size.1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        // An empty input region: clicks go to whatever is below.
        if let Ok(region) = Region::new(&self.compositor) {
            layer.set_input_region(Some(region.wl_region()));
        }
        layer.commit();
        Surface {
            layer,
            session,
            size,
            configured: false,
            scale: 1,
            buffer: None,
        }
    }

    fn draw(&mut self, scene: &Scene, style: &Style, painter: &mut Painter) {
        let Some(s) = self.surface.as_mut() else {
            return;
        };
        if !s.configured {
            return;
        }
        let mut scale = s.scale.max(1) as u32;
        if scale != 1 && s.layer.set_buffer_scale(scale).is_err() {
            scale = 1;
        }
        let (w, h) = (s.size.0 * scale, s.size.1 * scale);
        let created =
            self.pool
                .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888);
        let (buffer, data) = match created {
            Ok(created) => created,
            Err(e) => {
                tracing::warn!("cannot allocate an overlay buffer: {e}");
                return;
            }
        };
        let mut canvas = Canvas::new(data, w, h);
        let k = (style.scale * f64::from(scale)) as f32;
        painter.paint(scene, &mut canvas, k, anchor(style.position).align);
        let surface = s.layer.wl_surface();
        if surface.version() >= 4 {
            surface.damage_buffer(0, 0, w as i32, h as i32);
        } else {
            surface.damage(0, 0, s.size.0 as i32, s.size.1 as i32);
        }
        if let Err(e) = buffer.attach_to(surface) {
            tracing::warn!("cannot attach the overlay buffer: {e}");
            return;
        }
        s.layer.commit();
        s.buffer = Some(buffer);
        self.flush();
    }

    /// Whether `layer` is the current surface.
    fn current(&mut self, surface: &wl_surface::WlSurface) -> Option<&mut Surface> {
        self.surface
            .as_mut()
            .filter(|s| s.layer.wl_surface() == surface)
    }
}

impl Window for Wayland {
    fn watch(&mut self, handle: &LoopHandle<'static, Host<Wayland>>) -> Result<(), OverlayError> {
        let queue = self
            .queue
            .take()
            .ok_or_else(|| error("the Wayland queue is already watched"))?;
        WaylandSource::new(self.conn.clone(), queue)
            .insert(handle.clone())
            .map_err(|e| error(format!("cannot watch the Wayland connection: {}", e.error)))?;
        Ok(())
    }

    fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn present(&mut self, scene: Option<&Scene>, style: &Style, painter: &mut Painter) {
        let Some(scene) = scene else {
            // Destroying the surface unmaps it; the next session makes a new one.
            if self.surface.take().is_some() {
                self.flush();
            }
            return;
        };
        if self
            .surface
            .as_ref()
            .is_some_and(|s| s.session != scene.session)
        {
            self.surface = None;
        }
        if self.surface.is_none() {
            if self.closed == Some(scene.session) {
                return;
            }
            self.surface = Some(self.create(scene.session, style));
            self.flush();
            return;
        }
        self.draw(scene, style, painter);
    }

    fn rebuild(&mut self) {
        self.surface = None;
        self.closed = None;
        self.flush();
    }

    fn alive(&self) -> bool {
        self.alive
    }

    fn close(&mut self) {
        self.surface = None;
        let _ = self.conn.flush();
    }
}

impl CompositorHandler for Host<Wayland> {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if let Some(s) = self.win.current(surface) {
            s.scale = factor;
            self.win.dirty = true;
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Host<Wayland> {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.win.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Host<Wayland> {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        let win = &mut self.win;
        if let Some(session) = win.current(layer.wl_surface()).map(|s| s.session) {
            win.closed = Some(session);
            win.surface = None;
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        // The size asked for is fixed and anchored to one edge only, so the
        // compositor's suggestion never differs; acking is done by the toolkit.
        if let Some(s) = self.win.current(layer.wl_surface()) {
            s.configured = true;
            self.win.dirty = true;
        }
    }
}

impl ShmHandler for Host<Wayland> {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.win.shm
    }
}

impl ProvidesRegistryState for Host<Wayland> {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.win.registry
    }

    registry_handlers![OutputState];
}

delegate_compositor!(Host<Wayland>);
delegate_output!(Host<Wayland>);
delegate_shm!(Host<Wayland>);
delegate_layer!(Host<Wayland>);
delegate_registry!(Host<Wayland>);

/// Reads the globals once, nothing bound.
struct Probe;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Probe,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Probe>,
    ) {
    }
}

/// Whether the Wayland compositor offers layer-shell; `None` when there is
/// no compositor to ask.
pub(super) fn probe() -> Option<bool> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, _queue) = registry_queue_init::<Probe>(&conn).ok()?;
    Some(
        globals
            .contents()
            .with_list(|list| list.iter().any(|g| g.interface == "zwlr_layer_shell_v1")),
    )
}
