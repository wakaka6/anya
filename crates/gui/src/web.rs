//! Serves the settings page, the tray panel and their JSON API over loopback
//! HTTP, to the desktop window and to a browser alike.
//!
//! # Who may ask
//!
//! The page shows engine credentials and can start capture, so every request
//! proves it came from the link Anya handed out:
//!
//! - Each run makes a random key. [`WebServer::url`] carries it as `?key=`;
//!   a request with the right key gets an HttpOnly, SameSite=Strict cookie
//!   (`anya_key_<port>`, so instances on other ports keep their own) and a
//!   303 to the same path without the query. Keys compare in constant time.
//! - `Host` must be the listen address exactly, so a site that points its own
//!   name at 127.0.0.1 is refused (DNS rebinding).
//! - Anything but `GET` must carry `Origin: http://<listen address>` (CSRF).
//! - Headers must arrive within 10 s; bodies are capped per route.
//! - Refusals are plain text in the language of `[ui] language`.
//!
//! # Routes
//!
//! Static, `GET`: `/` (settings), `/panel.html` (tray panel), `/app.css`,
//! `/common.js`, `/app.js`, `/panel.js`, `/icon.svg`.
//!
//! JSON API. Errors are `{"error": "<translated text>"}` with a 4xx/5xx status.
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `GET /api/i18n` | | `{locale, text: {key: text}}`, the whole catalog |
//! | `GET /api/state` | | `{locale, shell, info, default_address, config, engines, positions, languages, ui_languages, devices, devices_error?}` |
//! | `GET /api/status` | | `{status, pill: {state, label}, detail, subscription: {state, line}, stats, activity}` |
//! | `POST /api/settings` | the whole config | `{state, warning?}`; 400 `{error, invalid: [{fields, message}]}`; 500 when writing fails |
//! | `POST /api/dictation` | `{on}` | `{state, warning?}` |
//! | `POST /api/continuous` | `{on}` | `{state, warning?}`; switches the running service too |
//! | `POST /api/engine/ready` | `{id, settings}` | `{ready}` for unsaved engine settings |
//! | `POST /api/hotkey/check` | `{value, role}` | `{value}` canonical; 400 with the reason |
//! | `POST /api/hotkey/record` | | `{value}` or `{value: null}` (Esc, time out); 409 busy; 500 when the OS recorder fails |
//! | `POST /api/hotkey/suspend` | `{active}` | 204; global hotkeys pause while the page records keys itself |
//! | `POST /api/open` | `{url}` | 204; app shell and engine docs links only, else 403 |
//! | `POST /api/pick-file` | `{path}` | `{path}` (`~/…` under home, `""` cancelled); app shell only, else 404 |
//! | `POST /api/window` | `{action: "settings" \| "quit"}` | 204, then acts; app shell only, else 404 |
//! | `POST /api/copy` | `{text}` | 204; puts text on the clipboard |

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anya_i18n::Catalog;
use anya_runtime::Control;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use crate::Shell;

mod api;
mod assets;
mod auth;
mod reply;
mod view;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

/// How long a client may take to send its request headers.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the page could not be served.
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    /// The address could not be bound.
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        /// The address asked for.
        addr: SocketAddr,
        /// What failed.
        source: std::io::Error,
    },
    /// The system gave no randomness for the key.
    #[error("cannot make a key: {0}")]
    Random(String),
}

/// The running page server. Dropping it stops accepting; [`WebServer::stop`]
/// also ends open connections and waits for that.
#[derive(Debug)]
pub struct WebServer {
    addr: SocketAddr,
    key: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl WebServer {
    /// The settings page's link, signing in with this run's key:
    /// `http://127.0.0.1:<port>/?key=<key>`.
    pub fn url(&self) -> String {
        format!("http://{}/?key={}", self.addr, self.key)
    }

    /// The tray panel's link, signing in the same way.
    pub fn panel_url(&self) -> String {
        format!("http://{}/panel.html?key={}", self.addr, self.key)
    }

    /// The address it listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops listening, closes every connection and waits for that.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for WebServer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Serves the page for `shell` on `addr` (`127.0.0.1:0` picks a port).
/// Must be called inside a Tokio runtime; the server runs on it until stopped.
pub async fn serve(
    control: Arc<dyn Control>,
    shell: Arc<dyn Shell>,
    addr: SocketAddr,
) -> Result<WebServer, WebError> {
    start(control, shell, addr, HEADER_TIMEOUT).await
}

async fn start(
    control: Arc<dyn Control>,
    shell: Arc<dyn Shell>,
    addr: SocketAddr,
    header_timeout: Duration,
) -> Result<WebServer, WebError> {
    let bind = |source| WebError::Bind { addr, source };
    let listener = TcpListener::bind(addr).await.map_err(bind)?;
    let addr = listener.local_addr().map_err(bind)?;
    let key = auth::new_key()?;
    let page = Arc::new(Page {
        control,
        shell,
        host: addr.to_string(),
        cookie: format!("anya_key_{}", addr.port()),
        key: key.clone(),
        catalog: Mutex::new(None),
        recording: AtomicBool::new(false),
        suspended: AtomicBool::new(false),
    });
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(accept(listener, page, header_timeout, stopped));
    Ok(WebServer {
        addr,
        key,
        stop: Some(stop),
        task: Some(task),
    })
}

/// What every request handler shares.
struct Page {
    control: Arc<dyn Control>,
    shell: Arc<dyn Shell>,
    /// The listen address as `Host` and `Origin` must name it.
    host: String,
    /// The cookie carrying the key.
    cookie: String,
    key: String,
    /// The catalog for the `[ui] language` it was made for.
    catalog: Mutex<Option<(String, Arc<Catalog>)>>,
    /// The OS hotkey recorder is running for a request.
    recording: AtomicBool,
    /// The page asked to pause global hotkeys and has not resumed them.
    suspended: AtomicBool,
}

impl Page {
    /// The catalog for the language in effect now, made once per setting:
    /// `system` asks the OS, which is too slow for every poll.
    fn catalog(&self) -> Arc<Catalog> {
        let setting = self.control.config().ui.language;
        let mut cached = lock(&self.catalog);
        match &*cached {
            Some((s, catalog)) if *s == setting => catalog.clone(),
            _ => {
                let catalog = Arc::new(Catalog::for_setting(&setting));
                *cached = Some((setting, catalog.clone()));
                catalog
            }
        }
    }
}

/// Locks `m`, taking the value even if a holder panicked: every value kept
/// here stays consistent between statements.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

async fn accept(
    listener: TcpListener,
    page: Arc<Page>,
    header_timeout: Duration,
    mut stopped: oneshot::Receiver<()>,
) {
    // Dropping the set when the loop ends aborts every open connection.
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let stream = tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    // Out of file descriptors and the like: wait, do not spin.
                    tracing::warn!("settings page cannot accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let page = page.clone();
        connections.spawn(async move {
            let service = service_fn(move |req| {
                let page = page.clone();
                async move { Ok::<_, std::convert::Infallible>(api::handle(&page, req).await) }
            });
            let served = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(header_timeout)
                .max_buf_size(64 * 1024)
                .serve_connection(TokioIo::new(stream), service)
                .await;
            if let Err(e) = served {
                tracing::debug!("settings page connection: {e}");
            }
        });
    }
}
