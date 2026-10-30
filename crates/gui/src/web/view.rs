//! What `/api/state` and `/api/status` answer: the runtime's data, shaped
//! for the page and with every word already translated.

use anya_config::{Config, DEFAULT_ADDRESS, EngineRole, LANGUAGES, POSITIONS};
use anya_core::audio::DeviceKind;
use anya_i18n::{Catalog, LOCALES};
use anya_protocol::{FieldKind, Manifest};
use anya_runtime::{
    DebugEvent, Delivered, Dictation, Entry, HotkeyRole, InstalledPlugin, Lan, MarketPlugin,
    Notice, PluginLoad, Subscription,
};
use serde_json::{Value, json};

use super::Page;

/// The activity list shows this many of the latest entries.
const ACTIVITY_SHOWN: usize = 100;

/// Everything the settings page and the panel need to draw their forms.
pub(super) fn state(page: &Page, catalog: &Catalog) -> Value {
    let config = canonical(page, page.control.config());
    let (devices, devices_error) = devices(page, catalog);
    json!({
        "locale": catalog.locale(),
        "shell": page.shell.kind(),
        "info": page.control.info(),
        "default_address": DEFAULT_ADDRESS,
        "engines": engines(page, &config, catalog),
        "positions": positions(&config.overlay.position, catalog),
        "languages": languages(&config.recognition.language, catalog),
        "ui_languages": ui_languages(&config.ui.language, catalog),
        "devices": devices,
        "devices_error": devices_error,
        "lan": lan(&page.control.status().lan, catalog),
        "clients": page.control.clients(),
        "plugins": plugins(&page.control.plugins(), catalog),
        "config": config,
    })
}

/// Installed plugins as the page lists them: a name, a state and a line
/// saying it in the catalog's language.
pub(super) fn plugins(list: &[InstalledPlugin], catalog: &Catalog) -> Vec<Value> {
    list.iter()
        .map(|p| {
            let (state, line) = match &p.load {
                PluginLoad::HostDown => (
                    "missing",
                    catalog.t("gui.plugins.state_host_down").to_owned(),
                ),
                PluginLoad::Off => ("off", catalog.t("gui.plugins.state_off").to_owned()),
                PluginLoad::Loading => {
                    ("loading", catalog.t("gui.plugins.state_loading").to_owned())
                }
                PluginLoad::Ready => ("ok", catalog.t("gui.plugins.state_ready").to_owned()),
                PluginLoad::Failed { detail } => (
                    "missing",
                    catalog.tf("gui.plugins.state_failed", &[("detail", detail)]),
                ),
            };
            let name = match p.title.as_ref().map(|t| t.get(catalog.locale())) {
                Some(n) if !n.is_empty() => n,
                _ => p.id.as_str(),
            };
            json!({
                "id": p.id, "name": name, "version": p.version,
                "enabled": p.enabled, "state": state, "line": line,
                "description": p.description.as_ref().map(|d| d.get(catalog.locale())).unwrap_or(""),
            })
        })
        .collect()
}

/// The registry's plugins as the page lists them.
pub(super) fn market(list: &[MarketPlugin], catalog: &Catalog) -> Vec<Value> {
    list.iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": match p.name.get(catalog.locale()) { "" => p.id.as_str(), n => n },
                "description": p.description.get(catalog.locale()),
                "version": p.version,
                "streaming": p.streaming,
                "permissions": p.permissions,
            })
        })
        .collect()
}

/// The LAN listener as the page shows it: a state, a line, and for a
/// listening one the URL and certificate fingerprint.
fn lan(lan: &Lan, catalog: &Catalog) -> Value {
    match lan {
        Lan::Off => json!({ "state": "off", "line": catalog.t("gui.lan.off") }),
        Lan::Listening { url, fingerprint } => json!({
            "state": "ok",
            "line": catalog.tf("gui.lan.listening", &[("url", url)]),
            "url": url,
            "fingerprint": fingerprint,
        }),
        Lan::Down { detail } => json!({
            "state": "missing",
            "line": catalog.tf("gui.lan.down", &[("detail", detail)]),
        }),
    }
}

/// `config` as the page edits it: hotkeys in canonical names (so an
/// unchanged list compares equal), each role's engine id resolved.
fn canonical(page: &Page, mut config: Config) -> Config {
    let control = &page.control;
    if let Ok(value) = control.check_hotkeys(&config.dictation.hotkeys, HotkeyRole::Dictation) {
        config.dictation.hotkeys = value;
    }
    if let Ok(value) = control.check_hotkeys(&config.continuous.hotkey, HotkeyRole::Continuous) {
        config.continuous.hotkey = value;
    }
    for role in EngineRole::ALL {
        let id = config.recognition.engine_id(role).to_owned();
        config.recognition.set_engine(role, &id);
    }
    config
}

fn engines(page: &Page, config: &Config, catalog: &Catalog) -> Vec<Value> {
    let locale = catalog.locale();
    let mut out: Vec<Value> = page
        .control
        .engines()
        .into_iter()
        .map(|e| engine(&e.manifest, e.ready, e.enabled, locale))
        .collect();
    // An engine the config names for a role but Anya lacks stays visible, so
    // the user sees what is chosen; saving it is refused with the reason.
    for current in config.recognition.engines_in_use() {
        if out.iter().any(|e| e["id"] == current) {
            continue;
        }
        out.push(json!({
            "id": current,
            "title": current,
            "blurb": catalog.t("gui.engine.custom"),
            "docs": null,
            "streaming": false,
            "ready": false,
            "custom": true,
            "plugin": false,
            "enabled": null,
            "fields": [],
        }));
    }
    out
}

fn engine(m: &Manifest, ready: bool, enabled: Option<bool>, locale: &str) -> Value {
    let title = match m.title.get(locale) {
        "" => m.id.as_str(),
        t => t,
    };
    let fields: Vec<Value> = m
        .fields
        .iter()
        .map(|f| {
            let (kind, options) = match &f.kind {
                FieldKind::Text => ("text", Vec::new()),
                FieldKind::Secret => ("secret", Vec::new()),
                FieldKind::Choice { options } => ("choice", options.clone()),
            };
            json!({
                "key": f.key,
                "label": match f.label.get(locale) { "" => f.key.as_str(), l => l },
                "help": f.help.get(locale),
                "kind": kind,
                "options": options,
                "required": f.required,
                "advanced": f.advanced,
            })
        })
        .collect();
    json!({
        "id": m.id,
        "title": title,
        "blurb": m.blurb.get(locale),
        "docs": m.docs,
        "streaming": m.streaming,
        "ready": ready,
        "custom": false,
        "plugin": enabled.is_none(),
        "enabled": enabled,
        "fields": fields,
    })
}

fn devices(page: &Page, catalog: &Catalog) -> (Vec<Value>, Option<String>) {
    match page.control.devices() {
        Ok(list) => (
            list.into_iter()
                .map(|d| {
                    let kind = match d.kind {
                        DeviceKind::Microphone => "microphone",
                        DeviceKind::Loopback => "loopback",
                    };
                    json!({ "id": d.id, "name": d.name, "kind": kind })
                })
                .collect(),
            None,
        ),
        Err(detail) => (
            Vec::new(),
            Some(catalog.tf("gui.device.error", &[("detail", &detail)])),
        ),
    }
}

fn option(value: &str, label: &str) -> Value {
    json!({ "value": value, "label": label })
}

/// The overlay positions; a configured unknown one is kept, shown as itself.
fn positions(current: &str, catalog: &Catalog) -> Vec<Value> {
    let mut out: Vec<Value> = POSITIONS
        .iter()
        .map(|p| option(p, position_key(p).map_or(*p, |k| catalog.t(k))))
        .collect();
    if !POSITIONS.contains(&current) {
        out.push(option(current, current));
    }
    out
}

fn position_key(position: &str) -> Option<&'static str> {
    Some(match position {
        "notch" => "gui.position.notch",
        "top-left" => "gui.position.top_left",
        "top-center" => "gui.position.top_center",
        "top-right" => "gui.position.top_right",
        "bottom-left" => "gui.position.bottom_left",
        "bottom-center" => "gui.position.bottom_center",
        "bottom-right" => "gui.position.bottom_right",
        _ => return None,
    })
}

/// Recognition languages; a configured tag outside the list is kept, so
/// saving never rewrites it.
fn languages(current: &str, catalog: &Catalog) -> Vec<Value> {
    let mut out: Vec<Value> = LANGUAGES
        .iter()
        .map(|code| match (code.is_empty(), language_key(code)) {
            (true, _) => option("", catalog.t("gui.language.auto")),
            (false, Some(key)) => option(
                code,
                &catalog.tf(
                    "gui.language.named",
                    &[("name", catalog.t(key)), ("code", code)],
                ),
            ),
            (false, None) => option(code, code),
        })
        .collect();
    let current = current.trim();
    if !LANGUAGES.contains(&current) {
        out.push(option(current, current));
    }
    out
}

fn language_key(code: &str) -> Option<&'static str> {
    Some(match code {
        "zh-CN" => "gui.language.zh_cn",
        "en-US" => "gui.language.en_us",
        "ja-JP" => "gui.language.ja_jp",
        "ko-KR" => "gui.language.ko_kr",
        "fr-FR" => "gui.language.fr_fr",
        "de-DE" => "gui.language.de_de",
        "es-ES" => "gui.language.es_es",
        "ru-RU" => "gui.language.ru_ru",
        _ => return None,
    })
}

/// The interface languages: the system's, then each shipped one by its own
/// name; a configured tag outside them is kept.
fn ui_languages(current: &str, catalog: &Catalog) -> Vec<Value> {
    let name = |tag: &str| match tag {
        "zh-CN" => Some("gui.ui_language.zh_cn"),
        "en" => Some("gui.ui_language.en"),
        _ => None,
    };
    let system = Catalog::for_setting("system").locale();
    let mut out = vec![option(
        "system",
        &catalog.tf(
            "gui.ui_language.system",
            &[("language", name(system).map_or(system, |k| catalog.t(k)))],
        ),
    )];
    out.extend(
        LOCALES
            .iter()
            .map(|tag| option(tag, name(tag).map_or(*tag, |k| catalog.t(k)))),
    );
    let current = current.trim();
    if current != "system" && !current.is_empty() && !LOCALES.contains(&current) {
        out.push(option(current, current));
    }
    out
}

/// The live state, polled a few times a second.
pub(super) fn status(page: &Page, catalog: &Catalog) -> Value {
    let status = page.control.status();
    let config = page.control.config();
    let activity = page.control.activity();
    let config_broken = config_broken(&activity, page.started);
    let engines = page.control.engines();
    let chosen = config
        .recognition
        .engine_id(EngineRole::Dictation)
        .to_owned();
    let engine = engines.iter().find(|e| e.manifest.id == chosen);
    let engine_ready = engine.is_some_and(|e| e.ready);
    let (pill, label) = match &status.dictation {
        Dictation::Recording => ("recording", "gui.state.recording"),
        Dictation::Stopping => ("stopping", "gui.state.stopping"),
        Dictation::Failed { .. } => ("failed", "gui.state.failed"),
        Dictation::Idle if status.finishing > 0 => ("finishing", "gui.state.finishing"),
        Dictation::Idle if config_broken => ("config_broken", "gui.state.config_broken"),
        Dictation::Idle if !engine_ready => ("not_ready", "gui.state.not_ready"),
        Dictation::Idle if !config.dictation.enabled => ("off", "gui.state.off"),
        Dictation::Idle => ("idle", "gui.state.idle"),
    };
    let mut detail = Vec::new();
    let mut failure = Value::Null;
    if let Dictation::Failed { error, detail: d } = &status.dictation {
        detail.push(catalog.error(*error, d.as_deref()));
        failure = json!({ "text": catalog.error(*error, None), "detail": d });
    }
    if status.finishing > 0 {
        detail.push(catalog.tf(
            "gui.state.finishing_count",
            &[("count", &status.finishing.to_string())],
        ));
    }
    let subscription = match &status.subscription {
        Subscription::Listening { url, subscribers } => json!({
            "state": "ok",
            "url": url,
            "subscribers": subscribers,
            "line": catalog.tf(
                "gui.subscription.listening",
                &[("url", url), ("count", &subscribers.to_string())],
            ),
        }),
        Subscription::Down { detail } => json!({
            "state": "missing",
            "line": catalog.tf("gui.subscription.down", &[("detail", detail)]),
        }),
    };
    // Debug lines are shown only while debug mode is on.
    let activity: Vec<&Entry> = activity
        .iter()
        .filter(|e| config.debug.enabled || !matches!(e, Entry::Debug { .. }))
        .collect();
    let skip = activity.len().saturating_sub(ACTIVITY_SHOWN);
    let activity: Vec<Value> = activity[skip..].iter().map(|e| entry(e, catalog)).collect();
    json!({
        "pill": { "state": pill, "label": catalog.t(label) },
        "detail": detail.join("  "),
        "failure": failure,
        "config_broken": config_broken,
        "engine": {
            "id": chosen,
            "title": engine.map_or(chosen.as_str(), |e| match e.manifest.title.get(catalog.locale()) {
                "" => e.manifest.id.as_str(),
                title => title,
            }),
            "ready": engine_ready,
        },
        "subscription": subscription,
        "lan": lan(&status.lan, catalog),
        "clients": page.control.clients(),
        "plugins": plugins(&page.control.plugins(), catalog),
        "stats": status.stats,
        "activity": activity,
        "status": status,
    })
}

/// Whether the config file went bad since `since` (unix ms) and has not been
/// read again: the latest of the two config notices is the broken one.
fn config_broken(activity: &[Entry], since: u64) -> bool {
    activity
        .iter()
        .rev()
        .filter_map(|e| match e {
            Entry::Notice { at, notice, .. } if *at >= since => Some(notice),
            _ => None,
        })
        .find(|n| matches!(n, Notice::ConfigBroken | Notice::ConfigReloaded))
        .is_some_and(|n| *n == Notice::ConfigBroken)
}

/// One activity line: `text` to show, `level` for its dot, `note` the place a
/// final text went, `detail` the untranslated specifics of a failure.
fn entry(entry: &Entry, catalog: &Catalog) -> Value {
    match entry {
        Entry::Final {
            at,
            source,
            text,
            delivered,
        } => {
            let note = match delivered {
                Delivered::Pasted => catalog.t("gui.delivered.pasted"),
                Delivered::Copied => catalog.t("gui.delivered.copied"),
                Delivered::Kept => catalog.t("gui.delivered.kept"),
            };
            json!({
                "at": at, "kind": "final", "source": source, "level": "ok",
                "text": text, "note": note,
            })
        }
        Entry::Failed {
            at,
            source,
            error,
            detail,
        } => json!({
            "at": at, "kind": "failed", "source": source, "level": "error",
            "text": catalog.error(*error, None),
            "detail": detail,
            "note": "",
        }),
        Entry::Debug { at, event } => {
            let text = match event {
                DebugEvent::KeyDown { combo } => {
                    catalog.tf("gui.debug.key_down", &[("combo", combo)])
                }
                DebugEvent::KeyUp { combo } => catalog.tf("gui.debug.key_up", &[("combo", combo)]),
                DebugEvent::Timing {
                    connect_ms,
                    first_text_ms,
                    final_ms,
                } => [
                    ("gui.debug.connect", connect_ms),
                    ("gui.debug.first_text", first_text_ms),
                    ("gui.debug.final", final_ms),
                ]
                .into_iter()
                .filter_map(|(key, ms)| Some(catalog.tf(key, &[("ms", &(*ms)?.to_string())])))
                .collect::<Vec<_>>()
                .join(catalog.t("gui.debug.separator")),
            };
            json!({
                "at": at, "kind": "debug", "source": null, "level": "debug",
                "text": text, "note": "",
            })
        }
        Entry::Notice { at, notice, detail } => {
            let (key, level) = match notice {
                Notice::ContinuousOn => ("gui.notice.continuous_on", "info"),
                Notice::ContinuousOff => ("gui.notice.continuous_off", "info"),
                Notice::DictationOn => ("gui.notice.dictation_on", "info"),
                Notice::DictationOff => ("gui.notice.dictation_off", "info"),
                Notice::ConfigReloaded => ("gui.notice.config_reloaded", "info"),
                Notice::ConfigBroken => ("gui.notice.config_broken", "warn"),
                Notice::HotwordsUnreadable => ("gui.notice.hotwords_unreadable", "warn"),
                Notice::DeliveryFailed => ("gui.notice.delivery_failed", "error"),
                Notice::PluginsUnavailable => ("gui.notice.plugins_unavailable", "warn"),
                Notice::OverlayUnavailable => ("gui.notice.overlay_unavailable", "warn"),
            };
            let text = match detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                Some(d) => catalog.tf(
                    "gui.notice.with_detail",
                    &[("notice", catalog.t(key)), ("detail", d)],
                ),
                None => catalog.t(key).to_owned(),
            };
            json!({
                "at": at, "kind": "notice", "source": null, "level": level,
                "text": text, "note": "",
            })
        }
    }
}
