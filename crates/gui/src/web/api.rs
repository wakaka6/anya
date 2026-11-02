//! The routes (listed in the module docs of [`super`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anya_config::{Config, DEFAULT_ADDRESS, DEFAULT_LAN_PORT, EngineRole};
use anya_i18n::Catalog;
use anya_protocol::{PairingOffer, Scope};
use anya_runtime::{
    ClientIssue, HotkeyRole, Invalid, PairError, PluginIssue, PreviewView, SaveError, Saved,
};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::{Method, Request, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::reply::{self, Reply};
use super::{Page, assets, auth, view};
use crate::{PRIVACY_SETTINGS_URL, ShellKind};

/// The largest settings or copy body.
const LARGE: usize = 1 << 20;
/// The largest body of any other route.
const SMALL: usize = 64 * 1024;
/// How long global hotkeys stay paused without a keepalive from the page.
const SUSPEND_HOLD: Duration = Duration::from_secs(15);
/// How long the OS recorder waits for a combination.
const RECORD_TIMEOUT: Duration = Duration::from_secs(15);
/// The panel's answer goes out before the window it lives in goes away.
const WINDOW_DELAY: Duration = Duration::from_millis(50);

pub(super) async fn handle(page: &Arc<Page>, req: Request<Incoming>) -> Reply {
    if let Some(refused) = auth::guard(page, &req) {
        return refused;
    }
    let path = req.uri().path().to_owned();
    let get = req.method() == Method::GET;
    if get && let Some((ty, body)) = assets::find(&path) {
        return reply::asset(ty, body.as_bytes());
    }
    let catalog = page.catalog();
    if get && let Some(name) = path.strip_prefix("/api/plugins/icon/") {
        return plugin_icon(page, &catalog, name.to_owned()).await;
    }
    let found = match (get, path.as_str()) {
        (true, "/api/i18n") => Some(i18n(&catalog)),
        (true, "/api/state") => Some(reply::json(StatusCode::OK, &view::state(page, &catalog))),
        (true, "/api/status") => Some(reply::json(StatusCode::OK, &view::status(page, &catalog))),
        (true, "/api/route") => Some(reply::json(StatusCode::OK, &view::route(page, &catalog))),
        (false, "/api/settings") => Some(save(page, &catalog, req).await),
        (false, "/api/dictation") => Some(switch(page, &catalog, req, Switch::Dictation).await),
        (false, "/api/continuous") => Some(switch(page, &catalog, req, Switch::Continuous).await),
        (false, "/api/dictation/retry") => {
            page.control.retry_dictation();
            Some(reply::empty())
        }
        (false, "/api/dictation/dismiss") => {
            page.control.dismiss_dictation();
            Some(reply::empty())
        }
        (false, "/api/overlay/preview") => Some(preview(page, &catalog, req).await),
        (false, "/api/pairing") => Some(start_pairing(page, &catalog)),
        (false, "/api/clients/scopes") => Some(set_scopes(page, &catalog, req).await),
        (false, "/api/clients/remove") => Some(remove_client(page, &catalog, req).await),
        (false, "/api/plugins/market") => Some(plugin_market(page, &catalog).await),
        (false, "/api/plugins/install") => Some(install_plugin(page, &catalog, req).await),
        (false, "/api/plugins/enable") => Some(enable_plugin(page, &catalog, req).await),
        (false, "/api/plugins/remove") => Some(remove_plugin(page, &catalog, req).await),
        (false, "/api/update/check") => Some(check_update(page, &catalog)),
        (false, "/api/update/upgrade") => Some(upgrade(page, &catalog).await),
        (false, "/api/engine/ready") => Some(engine_ready(page, &catalog, req).await),
        (false, "/api/hotkey/check") => Some(check_hotkey(page, &catalog, req).await),
        (false, "/api/hotkey/record") => Some(record_hotkey(page, &catalog).await),
        (false, "/api/hotkey/suspend") => Some(suspend(page, &catalog, req).await),
        (false, "/api/open") => Some(open(page, &catalog, req).await),
        (false, "/api/window") => Some(window(page, &catalog, req).await),
        (false, "/api/copy") => Some(copy_text(page, &catalog, req).await),
        _ => None,
    };
    found.unwrap_or_else(|| {
        if path.starts_with("/api/") {
            reply::error(StatusCode::NOT_FOUND, catalog.t("gui.api.unsupported"))
        } else {
            reply::text(StatusCode::NOT_FOUND, catalog.t("gui.web.not_found"))
        }
    })
}

/// The body as `T`, or the answer refusing it.
async fn body<T: DeserializeOwned>(
    req: Request<Incoming>,
    limit: usize,
    catalog: &Catalog,
) -> Result<T, Reply> {
    let bytes = match Limited::new(req.into_body(), limit).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => {
            return Err(reply::error(
                StatusCode::PAYLOAD_TOO_LARGE,
                catalog.t("gui.api.too_large"),
            ));
        }
        Err(_) => {
            return Err(reply::error(
                StatusCode::BAD_REQUEST,
                catalog.t("gui.api.bad_request"),
            ));
        }
    };
    serde_json::from_slice(&bytes)
        .map_err(|_| reply::error(StatusCode::BAD_REQUEST, catalog.t("gui.api.bad_request")))
}

fn i18n(catalog: &Catalog) -> Reply {
    let text: Value = serde_json::from_str(&catalog.to_json()).unwrap_or_else(|_| json!({}));
    reply::json(
        StatusCode::OK,
        &json!({ "locale": catalog.locale(), "text": text }),
    )
}

/// The answer to a save that was written: the new state, in the language
/// that may just have changed, and a warning when applying fell short.
fn applied(page: &Page, saved: Saved) -> Reply {
    // Saving re-registers the hotkeys, ending any pause the page asked for.
    resume(page);
    let catalog = page.catalog();
    let mut out = json!({ "state": view::state(page, &catalog) });
    if let Some(detail) = saved.hotkeys_failed {
        out["warning"] = catalog
            .tf("gui.save.hotkeys_failed", &[("detail", &detail)])
            .into();
    }
    reply::json(StatusCode::OK, &out)
}

fn refused(page: &Page, catalog: &Catalog, error: SaveError, config: &Config) -> Reply {
    match error {
        SaveError::Invalid(list) => {
            let invalid: Vec<Value> = list
                .iter()
                .map(|i| json!({ "fields": fields(page, i, config), "message": message(i, catalog) }))
                .collect();
            let messages: Vec<String> = list.iter().map(|i| message(i, catalog)).collect();
            let text = catalog.tf(
                "gui.save.invalid",
                &[("problems", &messages.join(catalog.t("gui.list_separator")))],
            );
            reply::json(
                StatusCode::BAD_REQUEST,
                &json!({ "error": text, "invalid": invalid }),
            )
        }
        SaveError::Write(detail) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.save.write_failed", &[("detail", &detail)]),
        ),
    }
}

/// The text of `invalid` in the catalog's language.
fn message(invalid: &Invalid, catalog: &Catalog) -> String {
    let (key, args) = invalid.message();
    let args: Vec<(&str, &str)> = args.iter().map(|(n, v)| (*n, v.as_str())).collect();
    catalog.tf(key, &args)
}

/// A coded message (`PluginIssue`, `PairError`) in the catalog's language.
fn coded(parts: (&'static str, Vec<(&'static str, String)>), catalog: &Catalog) -> String {
    let (key, args) = parts;
    let args: Vec<(&str, &str)> = args.iter().map(|(n, v)| (*n, v.as_str())).collect();
    catalog.tf(key, &args)
}

/// The config keys `invalid` is about, as the page names its fields.
fn fields(page: &Page, invalid: &Invalid, config: &Config) -> Vec<&'static str> {
    let hotkey = |field: &'static str, value: &str, role| {
        let found = page.control.check_hotkeys(value, role).err();
        (found.as_ref() == Some(invalid)).then_some(field)
    };
    match invalid {
        Invalid::HotkeyUnparsable { .. }
        | Invalid::HotkeyTextKey { .. }
        | Invalid::HotkeyFnFunctionKey { .. } => {
            let both = [
                hotkey(
                    "dictation.hotkeys",
                    &config.dictation.hotkeys,
                    HotkeyRole::Dictation,
                ),
                hotkey(
                    "continuous.hotkey",
                    &config.continuous.hotkey,
                    HotkeyRole::Continuous,
                ),
            ];
            match both.into_iter().flatten().collect::<Vec<_>>() {
                found if found.is_empty() => vec!["dictation.hotkeys"],
                found => found,
            }
        }
        Invalid::HotkeyTaken { .. } => vec!["continuous.hotkey"],
        Invalid::UnknownPosition { .. } => vec!["overlay.position"],
        Invalid::BadScale => vec!["overlay.scale"],
        Invalid::UnknownEngine { role, .. } => match role {
            EngineRole::Dictation => vec!["recognition.dictation"],
            EngineRole::Continuous => vec!["recognition.continuous"],
            EngineRole::Remote => vec!["recognition.remote"],
        },
        Invalid::BadAddress { .. } => vec!["subscription.address"],
        Invalid::BadPluginId { .. } => vec!["plugins.order", "plugins.settings"],
    }
}

/// Trims what the page typed, splits and dedupes hotwords, drops blank
/// engine settings; a blank address is the default one.
fn normalize(mut c: Config) -> Config {
    let trim = |s: &mut String| *s = s.trim().to_owned();
    trim(&mut c.ui.language);
    if c.ui.language.is_empty() {
        c.ui.language = "system".to_owned();
    }
    trim(&mut c.dictation.hotkeys);
    trim(&mut c.dictation.device);
    trim(&mut c.continuous.hotkey);
    trim(&mut c.continuous.device);
    trim(&mut c.recognition.dictation);
    trim(&mut c.recognition.continuous);
    trim(&mut c.recognition.remote);
    trim(&mut c.recognition.language);
    trim(&mut c.subscription.address);
    if c.subscription.address.is_empty() {
        c.subscription.address = DEFAULT_ADDRESS.to_owned();
    }
    if c.subscription.lan_port == 0 {
        c.subscription.lan_port = DEFAULT_LAN_PORT;
    }
    let mut words: Vec<String> = Vec::new();
    for word in c
        .recognition
        .hotwords
        .iter()
        .flat_map(|w| w.split([',', '，', '、', '\n']))
        .map(str::trim)
        .filter(|w| !w.is_empty())
    {
        if !words.iter().any(|x| x == word) {
            words.push(word.to_owned());
        }
    }
    c.recognition.hotwords = words;
    for settings in c.recognition.engines.values_mut() {
        *settings = clean_settings(std::mem::take(settings));
    }
    c
}

fn clean_settings(settings: BTreeMap<String, String>) -> BTreeMap<String, String> {
    settings
        .into_iter()
        .map(|(k, v)| (k, v.trim().to_owned()))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

async fn save(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let config: Config = match body(req, LARGE, catalog).await {
        Ok(c) => c,
        Err(refusal) => return refusal,
    };
    let config = normalize(config);
    match page.control.save(config.clone()) {
        Ok(saved) => applied(page, saved),
        Err(e) => refused(page, catalog, e, &config),
    }
}

#[derive(Clone, Copy)]
enum Switch {
    Dictation,
    Continuous,
}

#[derive(Deserialize)]
struct On {
    on: bool,
}

async fn switch(page: &Page, catalog: &Catalog, req: Request<Incoming>, which: Switch) -> Reply {
    let On { on } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    let result = match which {
        Switch::Dictation => page.control.set_dictation(on),
        Switch::Continuous => page.control.set_continuous(on),
    };
    match result {
        Ok(saved) => applied(page, saved),
        Err(e) => refused(page, catalog, e, &page.control.config()),
    }
}

#[derive(Deserialize)]
struct Preview {
    view: PreviewView,
}

async fn preview(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let Preview { view } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    match page.control.preview_overlay(view) {
        Ok(()) => reply::empty(),
        Err(issue) => reply::error(StatusCode::CONFLICT, catalog.t(issue.message())),
    }
}

#[derive(Deserialize)]
struct EngineSettings {
    id: String,
    #[serde(default)]
    settings: BTreeMap<String, String>,
}

async fn engine_ready(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let EngineSettings { id, settings } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    let ready = page.control.engine_ready(&id, &clean_settings(settings));
    reply::json(StatusCode::OK, &json!({ "ready": ready }))
}

#[derive(Deserialize)]
struct HotkeyCheck {
    value: String,
    role: HotkeyRole,
}

async fn check_hotkey(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let HotkeyCheck { value, role } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    match page.control.check_hotkeys(value.trim(), role) {
        Ok(value) => reply::json(StatusCode::OK, &json!({ "value": value })),
        Err(invalid) => reply::error(StatusCode::BAD_REQUEST, &message(&invalid, catalog)),
    }
}

/// Clears the recording flag however the request ends, a dropped
/// connection included.
struct Recording<'a>(&'a Page);

impl Drop for Recording<'_> {
    fn drop(&mut self) {
        self.0.recording.store(false, Ordering::SeqCst);
    }
}

async fn record_hotkey(page: &Arc<Page>, catalog: &Catalog) -> Reply {
    if page.recording.swap(true, Ordering::SeqCst) {
        return reply::error(StatusCode::CONFLICT, catalog.t("gui.api.record_busy"));
    }
    let _recording = Recording(page);
    let control = page.control.clone();
    let recorded = tokio::task::spawn_blocking(move || control.record_hotkey(RECORD_TIMEOUT)).await;
    match recorded {
        Ok(Ok(value)) => reply::json(StatusCode::OK, &json!({ "value": value })),
        Ok(Err(detail)) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.api.record_failed", &[("detail", &detail)]),
        ),
        Err(e) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.api.record_failed", &[("detail", &e.to_string())]),
        ),
    }
}

#[derive(Deserialize)]
struct Active {
    active: bool,
}

async fn suspend(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let Active { active } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    if active {
        page.suspended.store(true, Ordering::SeqCst);
        page.control.suspend_hotkeys(Some(SUSPEND_HOLD));
    } else {
        resume(page);
    }
    reply::empty()
}

/// Brings back global hotkeys the page paused, if it did.
fn resume(page: &Page) {
    if page.suspended.swap(false, Ordering::SeqCst) {
        page.control.suspend_hotkeys(None);
    }
}

#[derive(Deserialize)]
struct Url {
    url: String,
}

async fn open(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let Url { url } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    // Only engine documentation: the page must not become a way to launch
    // arbitrary links or files.
    let known = page
        .control
        .engines()
        .iter()
        .any(|e| e.manifest.docs.as_deref() == Some(url.as_str()));
    if page.shell.kind() != ShellKind::App || !known {
        return reply::error(StatusCode::FORBIDDEN, catalog.t("gui.api.open_refused"));
    }
    let shell = page.shell.clone();
    match tokio::task::spawn_blocking(move || shell.open_url(&url)).await {
        Ok(Ok(())) => reply::empty(),
        Ok(Err(detail)) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.api.open_failed", &[("detail", &detail)]),
        ),
        Err(e) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.api.open_failed", &[("detail", &e.to_string())]),
        ),
    }
}

#[derive(Deserialize)]
struct Action {
    action: String,
}

/// What a window action asks of the shell.
#[derive(Clone, Copy)]
enum Act {
    Settings,
    Quit,
    Privacy,
}

async fn window(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let Action { action } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    let act = match action.as_str() {
        "settings" => Act::Settings,
        "quit" => Act::Quit,
        "privacy" => Act::Privacy,
        _ => return reply::error(StatusCode::NOT_FOUND, catalog.t("gui.api.unsupported")),
    };
    if page.shell.kind() != ShellKind::App {
        return reply::error(StatusCode::NOT_FOUND, catalog.t("gui.api.unsupported"));
    }
    let shell = page.shell.clone();
    tokio::spawn(async move {
        tokio::time::sleep(WINDOW_DELAY).await;
        let _ = tokio::task::spawn_blocking(move || {
            match act {
                Act::Settings => shell.show_settings(),
                Act::Quit => shell.quit(),
                Act::Privacy => {
                    // The answer is long gone, so a failure has nowhere to go.
                    let _ = shell.open_url(PRIVACY_SETTINGS_URL);
                }
            }
        })
        .await;
    });
    reply::empty()
}

#[derive(Deserialize)]
struct Text {
    text: String,
}

async fn copy_text(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let Text { text } = match body(req, LARGE, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    match page.control.copy(&text) {
        Ok(()) => reply::empty(),
        Err(detail) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.api.copy_failed", &[("detail", &detail)]),
        ),
    }
}

/// A pairing code, and the QR code of what an app needs to use it.
fn start_pairing(page: &Page, catalog: &Catalog) -> Reply {
    let pairing = match page.control.start_pairing() {
        Ok(p) => p,
        Err(refusal) => {
            let detail = coded(PairError::message(&refusal), catalog);
            return reply::error(
                StatusCode::CONFLICT,
                &catalog.tf("gui.pairing.refused", &[("detail", &detail)]),
            );
        }
    };
    let Some(qr) = qr_svg(&pairing.offer) else {
        return reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            catalog.t("gui.pairing.qr_failed"),
        );
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    reply::json(
        StatusCode::OK,
        &json!({
            "code": pairing.code,
            "expires_in": pairing.expires_at.saturating_sub(now).div_ceil(1000),
            "url": pairing.offer.url,
            "fingerprint": pairing.offer.fingerprint,
            "qr": qr,
        }),
    )
}

/// `offer` as JSON in a QR code, drawn as an SVG that scales to its box.
fn qr_svg(offer: &PairingOffer) -> Option<String> {
    use qrcode::render::svg;
    let json = serde_json::to_string(offer).ok()?;
    let code = qrcode::QrCode::new(json.as_bytes()).ok()?;
    Some(
        code.render::<svg::Color>()
            .quiet_zone(true)
            .min_dimensions(200, 200)
            .build(),
    )
}

#[derive(Deserialize)]
struct ClientScopes {
    id: String,
    scopes: Vec<Scope>,
}

async fn set_scopes(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let ClientScopes { id, mut scopes } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    if scopes.contains(&Scope::Unknown) {
        return reply::error(StatusCode::BAD_REQUEST, catalog.t("gui.api.bad_request"));
    }
    scopes.sort();
    scopes.dedup();
    if !page.control.clients().iter().any(|c| c.id == id) {
        return reply::error(StatusCode::NOT_FOUND, catalog.t("gui.clients.unknown"));
    }
    match page.control.set_client_scopes(&id, scopes) {
        Ok(()) => reply::json(
            StatusCode::OK,
            &json!({ "clients": page.control.clients() }),
        ),
        Err(issue) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf(
                "gui.clients.failed",
                &[("detail", &coded(ClientIssue::message(&issue), catalog))],
            ),
        ),
    }
}

#[derive(Deserialize)]
struct ClientId {
    id: String,
}

async fn remove_client(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let ClientId { id } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    if !page.control.clients().iter().any(|c| c.id == id) {
        return reply::error(StatusCode::NOT_FOUND, catalog.t("gui.clients.unknown"));
    }
    match page.control.remove_client(&id) {
        Ok(()) => reply::json(
            StatusCode::OK,
            &json!({ "clients": page.control.clients() }),
        ),
        Err(issue) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf(
                "gui.clients.failed",
                &[("detail", &coded(ClientIssue::message(&issue), catalog))],
            ),
        ),
    }
}

/// The installed plugins, shaped for the page.
fn installed(page: &Page, catalog: &Catalog) -> Reply {
    reply::json(
        StatusCode::OK,
        &json!({ "plugins": view::plugins(&page.control.plugins(), catalog) }),
    )
}

/// What a plugin route does once it knows the plugin exists.
fn plugin_failed(catalog: &Catalog, detail: &str) -> Reply {
    reply::error(
        StatusCode::CONFLICT,
        &catalog.tf("gui.plugins.failed", &[("detail", detail)]),
    )
}

async fn plugin_market(page: &Page, catalog: &Catalog) -> Reply {
    let control = page.control.clone();
    let fetched = tokio::task::spawn_blocking(move || control.plugin_market()).await;
    match fetched {
        Ok(market) => reply::json(
            StatusCode::OK,
            &json!({
                "plugins": view::market(&market.plugins, catalog),
                "unreachable": market.unreachable.map(|issue| catalog.tf(
                    "gui.plugins.market_failed",
                    &[("detail", &coded(PluginIssue::message(&issue), catalog))],
                )),
            }),
        ),
        Err(e) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.plugins.market_failed", &[("detail", &e.to_string())]),
        ),
    }
}

async fn plugin_icon(page: &Page, catalog: &Catalog, name: String) -> Reply {
    let control = page.control.clone();
    match tokio::task::spawn_blocking(move || control.plugin_icon(&name)).await {
        Ok(Some(icon)) => reply::icon(icon.content_type, icon.bytes),
        _ => reply::error(StatusCode::NOT_FOUND, catalog.t("gui.api.unsupported")),
    }
}

async fn install_plugin(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let ClientId { id } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    let control = page.control.clone();
    let done = tokio::task::spawn_blocking(move || control.install_plugin(&id)).await;
    match done {
        Ok(Ok(())) => installed(page, catalog),
        Ok(Err(issue)) => plugin_failed(catalog, &coded(issue.message(), catalog)),
        Err(e) => plugin_failed(catalog, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct PluginSwitch {
    id: String,
    on: bool,
}

async fn enable_plugin(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let PluginSwitch { id, on } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    if !page.control.plugins().iter().any(|p| p.id == id) {
        return reply::error(StatusCode::NOT_FOUND, catalog.t("gui.plugins.unknown"));
    }
    let control = page.control.clone();
    let done = tokio::task::spawn_blocking(move || control.set_plugin_enabled(&id, on)).await;
    match done {
        Ok(Ok(())) => installed(page, catalog),
        Ok(Err(issue)) => plugin_failed(catalog, &coded(issue.message(), catalog)),
        Err(e) => plugin_failed(catalog, &e.to_string()),
    }
}

async fn remove_plugin(page: &Page, catalog: &Catalog, req: Request<Incoming>) -> Reply {
    let ClientId { id } = match body(req, SMALL, catalog).await {
        Ok(b) => b,
        Err(refusal) => return refusal,
    };
    if !page.control.plugins().iter().any(|p| p.id == id) {
        return reply::error(StatusCode::NOT_FOUND, catalog.t("gui.plugins.unknown"));
    }
    let control = page.control.clone();
    let done = tokio::task::spawn_blocking(move || control.remove_plugin(&id)).await;
    match done {
        Ok(Ok(())) => installed(page, catalog),
        Ok(Err(issue)) => plugin_failed(catalog, &coded(issue.message(), catalog)),
        Err(e) => plugin_failed(catalog, &e.to_string()),
    }
}

/// The update state as the page shows it right now.
fn update_now(page: &Page, catalog: &Catalog) -> Value {
    let checking = page.control.config().updates.check;
    view::update(&page.control.update(), checking, catalog)
}

/// Asks for a new check; the answer shows in the polled status.
fn check_update(page: &Page, catalog: &Catalog) -> Reply {
    page.control.check_update();
    reply::json(
        StatusCode::OK,
        &json!({ "update": update_now(page, catalog) }),
    )
}

/// Runs `brew upgrade`, which can take minutes, off the server's threads.
async fn upgrade(page: &Page, catalog: &Catalog) -> Reply {
    let control = page.control.clone();
    let done = tokio::task::spawn_blocking(move || control.upgrade()).await;
    match done {
        Ok(Ok(())) => reply::json(
            StatusCode::OK,
            &json!({
                "update": update_now(page, catalog),
                "message": catalog.t("gui.update.upgraded"),
            }),
        ),
        Ok(Err(issue)) => reply::error(StatusCode::CONFLICT, &view::upgrade_issue(&issue, catalog)),
        Err(e) => reply::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &catalog.tf("gui.update.issue_failed", &[("detail", &e.to_string())]),
        ),
    }
}
