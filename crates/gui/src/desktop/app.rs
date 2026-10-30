//! The event loop: the settings window, the tray panel, their webviews and
//! the tray icon.

use std::net::{Ipv4Addr, SocketAddr};
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

use anya_i18n::Catalog;
use anya_runtime::Control;
use tao::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::{Theme, Window, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use wry::{NewWindowResponse, WebView, WebViewBuilder};

use super::instance::Instance;
use super::placement::{self, Rect};
use super::shell::{self, AppShell};
use super::toggle::{Action, Toggle};
use super::{DesktopError, UserEvent, icons};
use crate::Shell;

const SETTINGS_SIZE: (f64, f64) = (780.0, 760.0);
const SETTINGS_MIN_SIZE: (f64, f64) = (420.0, 480.0);
const PANEL_WIDTH: f64 = 340.0;
/// The panel's height until its page says how tall it is, and the range the
/// page may ask for.
const PANEL_HEIGHT: f64 = 420.0;
const PANEL_HEIGHTS: (f64, f64) = (160.0, 720.0);
/// Between the tray icon and the panel, and the panel and the screen edge.
const PANEL_GAP: f64 = 6.0;
/// How often the tray's text looks for a changed language setting.
const RELABEL_EVERY: Duration = Duration::from_secs(2);

/// Where tray and menu events go. Their handlers can be set once per
/// process, so they forward to whichever event loop runs now.
static FORWARD: Mutex<Option<EventLoopProxy<UserEvent>>> = Mutex::new(None);
static HANDLERS: Once = Once::new();

fn forward(event: UserEvent) {
    let proxy = FORWARD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(proxy) = proxy.as_ref() {
        let _ = proxy.send_event(event);
    }
}

fn set_forward(proxy: Option<EventLoopProxy<UserEvent>>) {
    *FORWARD.lock().unwrap_or_else(PoisonError::into_inner) = proxy;
}

fn no_window(what: &str) -> impl FnOnce(String) -> DesktopError + '_ {
    move |e| DesktopError::NoWindow(format!("{what}: {e}"))
}

#[cfg(target_os = "macos")]
fn on_main_thread() -> bool {
    super::macos::on_main_thread()
}

#[cfg(not(target_os = "macos"))]
fn on_main_thread() -> bool {
    // The standard library names the thread that runs `main` so.
    std::thread::current().name() == Some("main")
}

pub(super) fn run(
    control: Arc<dyn Control>,
    catalog: Catalog,
    runtime: tokio::runtime::Handle,
    mut quit: tokio::sync::watch::Receiver<bool>,
    instance: Option<Instance>,
) -> Result<(), DesktopError> {
    if !on_main_thread() {
        return Err(DesktopError::NotMainThread);
    }
    // No display: tao would panic in the same call.
    #[cfg(target_os = "linux")]
    gtk::init().map_err(|e| no_window("GTK")(e.to_string()))?;

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // A menu bar app: no Dock icon.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    let shell = Arc::new(AppShell::new(
        proxy.clone(),
        catalog.t("tray.pick_file").to_owned(),
    ));
    let server = runtime.block_on(crate::web::serve(
        Arc::clone(&control),
        Arc::clone(&shell) as Arc<dyn Shell>,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    ))?;

    let origin = format!("http://{}", server.addr());
    let windows = Surface::settings(&event_loop, &catalog, &server.url(), &origin, &shell)
        .and_then(|settings| {
            let panel =
                Surface::panel(&event_loop, &catalog, &server.panel_url(), &origin, &shell)?;
            Ok((settings, panel))
        });
    let (settings, panel) = match windows {
        Ok(windows) => windows,
        Err(e) => {
            runtime.block_on(server.stop());
            return Err(e);
        }
    };

    set_forward(Some(proxy.clone()));
    HANDLERS.call_once(|| {
        TrayIconEvent::set_event_handler(Some(|e| forward(UserEvent::Tray(e))));
        MenuEvent::set_event_handler(Some(|e| forward(UserEvent::Menu(e))));
    });
    if let Some(instance) = &instance {
        let proxy = proxy.clone();
        instance.on_activate(move || {
            let _ = proxy.send_event(UserEvent::ShowSettings);
        });
    }
    let watcher = runtime.spawn(async move {
        // A dropped sender also ends the app: nothing is left to run it.
        let _ = quit.wait_for(|quit| *quit).await;
        let _ = proxy.send_event(UserEvent::Exit);
    });

    let mut app = App {
        language: control.config().ui.language,
        control,
        catalog,
        shell,
        settings,
        panel,
        panel_height: PANEL_HEIGHT,
        panel_anchor: None,
        tray: None,
        toggle: Toggle::default(),
        next_relabel: Instant::now() + RELABEL_EVERY,
        exiting: false,
        failure: None,
        #[cfg(target_os = "linux")]
        picker: None,
    };
    event_loop.run_return(|event, target, flow| {
        app.handle(event, target);
        *flow = if app.exiting {
            ControlFlow::Exit
        } else {
            ControlFlow::WaitUntil(app.next_relabel)
        };
    });

    watcher.abort();
    set_forward(None);
    drop(instance);
    let failure = app.failure.take();
    drop(app);
    runtime.block_on(server.stop());
    failure.map_or(Ok(()), Err)
}

/// A window and the page in it.
struct Surface {
    // Dropped before the window it lives in.
    _webview: WebView,
    window: Rc<Window>,
}

impl Surface {
    fn settings(
        target: &EventLoopWindowTarget<UserEvent>,
        catalog: &Catalog,
        url: &str,
        origin: &str,
        shell: &Arc<AppShell>,
    ) -> Result<Surface, DesktopError> {
        let builder = WindowBuilder::new()
            .with_title(catalog.t("tray.window_title"))
            .with_inner_size(LogicalSize::new(SETTINGS_SIZE.0, SETTINGS_SIZE.1))
            .with_min_inner_size(LogicalSize::new(SETTINGS_MIN_SIZE.0, SETTINGS_MIN_SIZE.1))
            // Shown once the tray icon is up.
            .with_visible(false);
        #[cfg(target_os = "macos")]
        let builder = {
            use tao::platform::macos::WindowBuilderExtMacOS;
            // Hidden-inset title bar: the page's 52 px header runs under the
            // traffic lights and drags the window (see `webview`).
            builder
                .with_titlebar_transparent(true)
                .with_title_hidden(true)
                .with_fullsize_content_view(true)
                .with_traffic_light_inset(LogicalPosition::new(20.0, 19.0))
                // The sidebar material shows through the page's sidebar.
                .with_transparent(true)
        };
        let window = Rc::new(
            builder
                .build(target)
                .map_err(|e| no_window("settings window")(e.to_string()))?,
        );
        #[cfg(target_os = "linux")]
        set_window_icon(&window, window.theme());
        let webview = webview(&window, url, origin, shell, true)?;
        #[cfg(target_os = "macos")]
        super::macos::backdrop(&window, super::macos::Backdrop::Sidebar);
        Ok(Surface {
            _webview: webview,
            window,
        })
    }

    fn panel(
        target: &EventLoopWindowTarget<UserEvent>,
        catalog: &Catalog,
        url: &str,
        origin: &str,
        shell: &Arc<AppShell>,
    ) -> Result<Surface, DesktopError> {
        let builder = WindowBuilder::new()
            .with_title(catalog.t("tray.window_title"))
            .with_inner_size(LogicalSize::new(PANEL_WIDTH, PANEL_HEIGHT))
            .with_resizable(false)
            .with_minimizable(false)
            .with_maximizable(false)
            .with_decorations(false)
            .with_always_on_top(true)
            .with_visible_on_all_workspaces(true)
            .with_visible(false);
        #[cfg(target_os = "linux")]
        let builder = {
            use tao::platform::unix::WindowBuilderExtUnix;
            builder.with_skip_taskbar(true)
        };
        // The menu material and its rounded corners show through the page.
        #[cfg(target_os = "macos")]
        let builder = builder.with_transparent(true);
        let window = Rc::new(
            builder
                .build(target)
                .map_err(|e| no_window("tray panel")(e.to_string()))?,
        );
        #[cfg(target_os = "macos")]
        super::macos::float_over_spaces(&window);
        let webview = webview(&window, url, origin, shell, false)?;
        #[cfg(target_os = "macos")]
        super::macos::backdrop(&window, super::macos::Backdrop::Menu);
        Ok(Surface {
            _webview: webview,
            window,
        })
    }
}

/// The page at `url` in `window`. Links away from `origin` open in the
/// browser. With `title_bar`, the page's header moves the window: it posts
/// `drag` on a press and `zoom` on a double click. Without, the page posts
/// `height:<px>` and the window takes that height. On macOS the page is told
/// a system material is behind it.
fn webview(
    window: &Rc<Window>,
    url: &str,
    origin: &str,
    shell: &Arc<AppShell>,
    title_bar: bool,
) -> Result<WebView, DesktopError> {
    let navigate = {
        let origin = origin.to_owned();
        let shell = Arc::clone(shell);
        move |url: String| {
            if shell::same_origin(&url, &origin) {
                return true;
            }
            if let Err(e) = shell.open_url(&url) {
                tracing::warn!(error = %e, "link not opened");
            }
            false
        }
    };
    let new_window = {
        let shell = Arc::clone(shell);
        move |url: String, _| {
            if let Err(e) = shell.open_url(&url) {
                tracing::warn!(error = %e, "link not opened");
            }
            NewWindowResponse::Deny
        }
    };
    // Until the page paints its own background the window shows through,
    // never WebKit's white.
    let mut builder = WebViewBuilder::new()
        .with_transparent(true)
        .with_url(url)
        .with_navigation_handler(navigate)
        .with_new_window_req_handler(new_window);
    #[cfg(target_os = "macos")]
    {
        builder = builder.with_initialization_script(super::macos::VIBRANT_SCRIPT);
    }
    if title_bar {
        let window = Rc::clone(window);
        builder = builder.with_ipc_handler(move |request| match request.body().as_str() {
            "drag" => {
                if let Err(e) = window.drag_window() {
                    tracing::debug!(error = %e, "window drag failed");
                }
            }
            "zoom" => window.set_maximized(!window.is_maximized()),
            other => tracing::debug!(message = other, "unknown page message"),
        });
    } else {
        let shell = Arc::clone(shell);
        builder = builder.with_ipc_handler(move |request| {
            match request
                .body()
                .strip_prefix("height:")
                .and_then(|h| h.parse::<f64>().ok())
            {
                Some(height) if height.is_finite() => {
                    shell.send(UserEvent::PanelHeight(height));
                }
                _ => tracing::debug!(message = request.body().as_str(), "unknown page message"),
            }
        });
    }
    #[cfg(target_os = "linux")]
    let built = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| DesktopError::NoWindow("window has no GTK container".to_owned()))?;
        builder.build_gtk(vbox)
    };
    #[cfg(not(target_os = "linux"))]
    let built = builder.build(window.as_ref());
    built.map_err(|e| no_window("webview")(e.to_string()))
}

/// The tray menu's items.
struct TrayMenu {
    menu: Menu,
    /// Linux panels give the app no clicks on the icon, only the menu.
    show_panel: Option<MenuItem>,
    open_settings: MenuItem,
    quit: MenuItem,
}

impl TrayMenu {
    fn new(catalog: &Catalog) -> Result<TrayMenu, String> {
        let menu = Menu::new();
        let show_panel = cfg!(target_os = "linux")
            .then(|| MenuItem::new(catalog.t("tray.show_panel"), true, None));
        if let Some(item) = &show_panel {
            menu.append(item).map_err(|e| e.to_string())?;
        }
        let open_settings = MenuItem::new(catalog.t("tray.open_settings"), true, None);
        let quit = MenuItem::new(catalog.t("tray.quit"), true, None);
        menu.append_items(&[&open_settings, &PredefinedMenuItem::separator(), &quit])
            .map_err(|e| e.to_string())?;
        Ok(TrayMenu {
            menu,
            show_panel,
            open_settings,
            quit,
        })
    }

    fn relabel(&self, catalog: &Catalog) {
        if let Some(item) = &self.show_panel {
            item.set_text(catalog.t("tray.show_panel"));
        }
        self.open_settings.set_text(catalog.t("tray.open_settings"));
        self.quit.set_text(catalog.t("tray.quit"));
    }
}

/// The macOS application menu. It is never shown (Anya has no menu bar),
/// but its key equivalents give the webviews' text fields Cmd+C, Cmd+V…
/// and the settings window Cmd+W.
#[cfg(target_os = "macos")]
fn edit_menu(catalog: &Catalog) -> Result<Menu, String> {
    use tray_icon::menu::Submenu;
    let edit = Submenu::with_items(
        catalog.t("tray.edit"),
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    )
    .map_err(|e| e.to_string())?;
    let window = Submenu::with_items(
        catalog.t("tray.window_menu"),
        true,
        &[&PredefinedMenuItem::close_window(None)],
    )
    .map_err(|e| e.to_string())?;
    let menu = Menu::with_items(&[&edit, &window]).map_err(|e| e.to_string())?;
    menu.init_for_nsapp();
    Ok(menu)
}

struct Tray {
    icon: TrayIcon,
    menu: TrayMenu,
    #[cfg(target_os = "macos")]
    _edit: Menu,
}

impl Tray {
    fn new(catalog: &Catalog, theme: Theme) -> Result<Tray, String> {
        let menu = TrayMenu::new(catalog)?;
        let png = if cfg!(target_os = "macos") {
            icons::TRAY_TEMPLATE
        } else {
            icons::tray(theme)
        };
        let image = tray_image(png)?;
        let builder = TrayIconBuilder::new()
            .with_tooltip(catalog.t("tray.tooltip"))
            .with_menu(Box::new(menu.menu.clone()))
            // Left click toggles the panel; the menu is on the right button.
            .with_menu_on_left_click(false);
        // macOS tints a template icon to suit the menu bar.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(image);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(image);
        // A Linux session without the appindicator library panics in it.
        let icon = panic::catch_unwind(AssertUnwindSafe(|| builder.build()))
            .map_err(|_| "the tray library is missing".to_owned())?
            .map_err(|e| e.to_string())?;
        Ok(Tray {
            icon,
            menu,
            #[cfg(target_os = "macos")]
            _edit: edit_menu(catalog)?,
        })
    }
}

fn tray_image(png: &[u8]) -> Result<tray_icon::Icon, String> {
    let rgba = icons::decode(png)?;
    tray_icon::Icon::from_rgba(rgba.pixels, rgba.width, rgba.height).map_err(|e| e.to_string())
}

/// Gives the window the app icon that suits the system appearance.
#[cfg(target_os = "linux")]
fn set_window_icon(window: &Window, theme: Theme) {
    match icons::decode(icons::app(theme)).and_then(|i| {
        tao::window::Icon::from_rgba(i.pixels, i.width, i.height).map_err(|e| e.to_string())
    }) {
        Ok(icon) => window.set_window_icon(Some(icon)),
        Err(e) => tracing::warn!(error = %e, "no window icon"),
    }
}

struct App {
    control: Arc<dyn Control>,
    language: String,
    catalog: Catalog,
    shell: Arc<AppShell>,
    // Dropped before the windows.
    tray: Option<Tray>,
    settings: Surface,
    panel: Surface,
    /// The panel's height, as its page last asked.
    panel_height: f64,
    /// Where the panel was last opened from, to keep it there as it resizes.
    panel_anchor: Option<Rect>,
    toggle: Toggle,
    next_relabel: Instant,
    exiting: bool,
    failure: Option<DesktopError>,
    /// The open file dialog, kept alive until it answers.
    #[cfg(target_os = "linux")]
    picker: Option<gtk::FileChooserNative>,
}

impl App {
    fn handle(&mut self, event: Event<'_, UserEvent>, target: &EventLoopWindowTarget<UserEvent>) {
        let now = Instant::now();
        match event {
            Event::NewEvents(StartCause::Init) => {
                match Tray::new(&self.catalog, self.settings.window.theme()) {
                    Ok(tray) => {
                        self.tray = Some(tray);
                        self.show_settings();
                    }
                    Err(e) => {
                        self.failure = Some(DesktopError::NoWindow(format!("tray icon: {e}")));
                        self.exiting = true;
                    }
                }
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                self.relabel();
                self.next_relabel = now + RELABEL_EVERY;
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } if window_id == self.settings.window.id() => {
                // Anya keeps running in the tray.
                self.settings.window.set_visible(false);
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Focused(false),
                ..
            } if window_id == self.panel.window.id() && self.toggle.blur(now) == Action::Hide => {
                self.panel.window.set_visible(false);
            }
            // The system appearance changed; the event names no real window.
            #[cfg(target_os = "linux")]
            Event::WindowEvent {
                event: WindowEvent::ThemeChanged(theme),
                ..
            } => self.retheme(theme),
            Event::UserEvent(UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            })) => match self.toggle.click(now) {
                Action::Show => {
                    let anchor = Rect {
                        x: rect.position.x,
                        y: rect.position.y,
                        width: f64::from(rect.size.width),
                        height: f64::from(rect.size.height),
                    };
                    self.show_panel(target, Some(anchor));
                }
                Action::Hide => self.panel.window.set_visible(false),
                Action::Nothing => {}
            },
            Event::UserEvent(UserEvent::Menu(event)) => self.menu(&event, target),
            Event::UserEvent(UserEvent::ShowSettings) => self.show_settings(),
            Event::UserEvent(UserEvent::PanelHeight(height)) => self.fit_panel(target, height),
            Event::UserEvent(UserEvent::Quit) => {
                self.control.quit();
                self.exiting = true;
            }
            Event::UserEvent(UserEvent::Exit) => self.exiting = true,
            #[cfg(target_os = "linux")]
            Event::UserEvent(UserEvent::PickFile {
                title,
                directory,
                reply,
            }) => self.pick_file(&title, directory, reply),
            _ => {}
        }
    }

    fn menu(&mut self, event: &MenuEvent, target: &EventLoopWindowTarget<UserEvent>) {
        let Some(tray) = &self.tray else { return };
        let items = &tray.menu;
        if items
            .show_panel
            .as_ref()
            .is_some_and(|i| event.id == *i.id())
        {
            // Opened from the menu, the panel goes by the pointer.
            let anchor = target.cursor_position().ok().map(|p| Rect {
                x: p.x,
                y: p.y,
                width: 0.0,
                height: 0.0,
            });
            self.toggle.shown();
            self.show_panel(target, anchor);
        } else if event.id == *items.open_settings.id() {
            self.show_settings();
        } else if event.id == *items.quit.id() {
            self.control.quit();
            self.exiting = true;
        }
    }

    /// Shows the panel by `anchor` (physical pixels), where it is known.
    fn show_panel(&mut self, target: &EventLoopWindowTarget<UserEvent>, anchor: Option<Rect>) {
        self.relabel();
        self.panel_anchor = anchor;
        self.place_panel(target);
        self.panel.window.set_visible(true);
        self.panel.window.set_focus();
    }

    /// Gives the panel the height its page needs, staying by its anchor.
    fn fit_panel(&mut self, target: &EventLoopWindowTarget<UserEvent>, height: f64) {
        let height = height.clamp(PANEL_HEIGHTS.0, PANEL_HEIGHTS.1).ceil();
        if (height - self.panel_height).abs() < 1.0 {
            return;
        }
        self.panel_height = height;
        self.panel
            .window
            .set_inner_size(LogicalSize::new(PANEL_WIDTH, height));
        self.place_panel(target);
    }

    fn place_panel(&self, target: &EventLoopWindowTarget<UserEvent>) {
        let size = (PANEL_WIDTH, self.panel_height);
        if let Some(position) = self
            .panel_anchor
            .and_then(|a| panel_position(target, a, size))
        {
            self.panel.window.set_outer_position(position);
        }
    }

    /// Opens the file dialog over the settings window without blocking the
    /// loop: the portal's dialog where there is one, GTK's otherwise.
    #[cfg(target_os = "linux")]
    fn pick_file(
        &mut self,
        title: &str,
        directory: Option<std::path::PathBuf>,
        reply: std::sync::mpsc::Sender<Option<std::path::PathBuf>>,
    ) {
        use gtk::prelude::{FileChooserExt, NativeDialogExt};
        use tao::platform::unix::WindowExtUnix;
        let dialog = gtk::FileChooserNative::new(
            Some(title),
            Some(self.settings.window.gtk_window()),
            gtk::FileChooserAction::Open,
            None,
            None,
        );
        dialog.set_modal(true);
        if let Some(directory) = directory {
            dialog.set_current_folder(directory);
        }
        dialog.connect_response(move |dialog, response| {
            let chosen = (response == gtk::ResponseType::Accept)
                .then(|| dialog.filename())
                .flatten();
            let _ = reply.send(chosen);
        });
        dialog.show();
        // A second request replaces the first, whose caller then hears
        // that it closed without an answer.
        if let Some(old) = self.picker.replace(dialog) {
            old.destroy();
        }
    }

    fn show_settings(&mut self) {
        self.toggle.hidden();
        self.panel.window.set_visible(false);
        let window = &self.settings.window;
        window.set_visible(true);
        window.set_minimized(false);
        window.set_focus();
    }

    /// Swaps the window and tray icons for the new system appearance.
    #[cfg(target_os = "linux")]
    fn retheme(&self, theme: Theme) {
        set_window_icon(&self.settings.window, theme);
        let Some(tray) = &self.tray else { return };
        if let Err(e) = tray_image(icons::tray(theme))
            .and_then(|image| tray.icon.set_icon(Some(image)).map_err(|e| e.to_string()))
        {
            tracing::warn!(error = %e, "tray icon not updated");
        }
    }

    /// Follows a changed `[ui] language` in the tray and the window titles.
    fn relabel(&mut self) {
        let language = self.control.config().ui.language;
        if language == self.language {
            return;
        }
        self.catalog = Catalog::for_setting(&language);
        self.language = language;
        let title = self.catalog.t("tray.window_title");
        self.settings.window.set_title(title);
        self.panel.window.set_title(title);
        self.shell.set_pick_title(self.catalog.t("tray.pick_file"));
        if let Some(tray) = &self.tray {
            tray.menu.relabel(&self.catalog);
            if let Err(e) = tray.icon.set_tooltip(Some(self.catalog.t("tray.tooltip"))) {
                tracing::debug!(error = %e, "tray tooltip not set");
            }
        }
    }
}

/// The panel's position for an anchor in physical pixels, in the logical
/// points of the screen the anchor is on.
fn panel_position(
    target: &EventLoopWindowTarget<UserEvent>,
    anchor: Rect,
    panel: (f64, f64),
) -> Option<LogicalPosition<f64>> {
    let center = (
        anchor.x + anchor.width / 2.0,
        anchor.y + anchor.height / 2.0,
    );
    target.available_monitors().find_map(|monitor| {
        let scale = monitor.scale_factor();
        let origin: PhysicalPosition<i32> = monitor.position();
        let size = monitor.size();
        let screen = Rect {
            x: f64::from(origin.x) / scale,
            y: f64::from(origin.y) / scale,
            width: f64::from(size.width) / scale,
            height: f64::from(size.height) / scale,
        };
        if !screen.contains((center.0 / scale, center.1 / scale)) {
            return None;
        }
        let logical = Rect {
            x: anchor.x / scale,
            y: anchor.y / scale,
            width: anchor.width / scale,
            height: anchor.height / scale,
        };
        let (x, y) = placement::place(logical, panel, screen, PANEL_GAP);
        Some(LogicalPosition::new(x, y))
    })
}
