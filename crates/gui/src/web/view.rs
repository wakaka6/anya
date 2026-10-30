//! What `/api/state`, `/api/status` and `/api/route` answer: the runtime's
//! data, shaped for the page and with every word already translated.

use anya_config::{Config, DEFAULT_ADDRESS, EngineRole, LANGUAGES, POSITIONS};
use anya_core::audio::DeviceKind;
use anya_i18n::{Catalog, LOCALES};
use anya_protocol::{EndReason, FieldKind, Manifest};
use anya_runtime::{
    DebugEvent, Delivered, Dictation, EngineInfo, Entry, HotkeyRole, InstalledPlugin, Lan,
    MarketPlugin, Notice, Origin, PluginLoad, RouteEvent, Sink, Step, Subscription, UpdateIssue,
    UpdateState, UpdateVia,
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
    let activity: Vec<Value> = activity[skip..]
        .iter()
        .map(|e| entry(e, &engines, catalog))
        .collect();
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
        "update": update(&page.control.update(), config.updates.check, catalog),
        "status": status,
    })
}

/// The update check as the page shows it: a `state`, the `line` saying it,
/// whether the tray panel gives it a `notice` (a failed check never
/// interrupts), and whether `can_upgrade` offers the action.
pub(super) fn update(state: &UpdateState, checking: bool, catalog: &Catalog) -> Value {
    let (name, line, notice, can_upgrade) = match state {
        UpdateState::Unknown if !checking => (
            "unknown",
            catalog.t("gui.update.off").to_owned(),
            false,
            false,
        ),
        UpdateState::Unknown => (
            "unknown",
            catalog.t("gui.update.unknown").to_owned(),
            false,
            false,
        ),
        UpdateState::Current => (
            "current",
            catalog.t("gui.update.current").to_owned(),
            false,
            false,
        ),
        UpdateState::Available {
            version,
            via: UpdateVia::Homebrew,
        } => (
            "available",
            catalog.tf("gui.update.available", &[("version", version)]),
            true,
            true,
        ),
        UpdateState::Available {
            version,
            via: UpdateVia::Other,
        } => (
            "available",
            catalog.tf("gui.update.available_other", &[("version", version)]),
            true,
            false,
        ),
        UpdateState::Upgrading { version } => (
            "upgrading",
            catalog.tf("gui.update.upgrading", &[("version", version)]),
            true,
            false,
        ),
        UpdateState::Restart { version } => (
            "restart",
            catalog.tf("gui.update.restart", &[("version", version)]),
            true,
            false,
        ),
        UpdateState::Failed { detail } => (
            "failed",
            catalog.tf("gui.update.failed", &[("detail", detail)]),
            false,
            false,
        ),
    };
    json!({ "state": name, "line": line, "notice": notice, "can_upgrade": can_upgrade })
}

/// Why an upgrade did not happen, in the catalog's language; brew's own
/// words stay as they came.
pub(super) fn upgrade_issue(issue: &UpdateIssue, catalog: &Catalog) -> String {
    match issue {
        UpdateIssue::NotHomebrew => catalog.t("gui.update.issue_not_homebrew").to_owned(),
        UpdateIssue::BrewMissing => catalog.t("gui.update.issue_brew_missing").to_owned(),
        UpdateIssue::NothingToInstall => catalog.t("gui.update.issue_nothing").to_owned(),
        UpdateIssue::Failed { detail } => {
            catalog.tf("gui.update.issue_failed", &[("detail", detail)])
        }
    }
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

/// The engine `id`'s title in the catalog's language; the id itself for an
/// engine that is not there any more.
fn engine_title(engines: &[EngineInfo], id: &str, catalog: &Catalog) -> String {
    engines
        .iter()
        .find(|e| e.manifest.id == id)
        .map(|e| e.manifest.title.get(catalog.locale()))
        .filter(|t| !t.is_empty())
        .unwrap_or(id)
        .to_owned()
}

/// The route page: the sessions open now and the latest messages, newest
/// last. Every label is translated here.
pub(super) fn route(page: &Page, catalog: &Catalog) -> Value {
    let route = page.control.route();
    let engines = page.control.engines();
    let engine = |id: &str| json!({ "id": id, "title": engine_title(&engines, id, catalog) });
    let live: Vec<Value> = route
        .live
        .iter()
        .map(|r| {
            json!({
                "session": r.session,
                "origin": origin(&r.origin, catalog),
                "engine": engine(&r.engine),
                "streaming": r.streaming,
                "level": r.level,
                "text": r.text,
                "sinks": r.sinks.iter().map(|s| sink(s, catalog)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let log: Vec<Value> = route
        .log
        .iter()
        .flat_map(|e| rows(e, &engine_title(&engines, &e.engine, catalog), catalog))
        .collect();
    json!({ "live": live, "log": log })
}

/// Where a session's audio comes from: a `key` that names the node, its
/// `label`, and the use it is (`role`).
fn origin(origin: &Origin, catalog: &Catalog) -> Value {
    let named = |device: &Option<String>, key: &str| match device.as_deref().map(str::trim) {
        Some(d) if !d.is_empty() => d.to_owned(),
        _ => catalog.t(key).to_owned(),
    };
    let (key, label, role) = match origin {
        Origin::Dictation { device } => (
            "dictation".to_owned(),
            named(device, "gui.route.microphone"),
            "gui.route.role_dictation",
        ),
        Origin::Continuous { device } => (
            "continuous".to_owned(),
            named(device, "gui.route.system_sound"),
            "gui.route.role_continuous",
        ),
        Origin::Remote { client } => (
            format!("remote:{client}"),
            if client.is_empty() {
                catalog.t("gui.route.paired_app").to_owned()
            } else {
                client.clone()
            },
            "gui.route.role_remote",
        ),
    };
    json!({ "key": key, "label": label, "role": catalog.t(role), "local": matches!(origin, Origin::Dictation { .. }) })
}

/// Where a session's text goes: a `key` that names the node, and its `label`.
fn sink(sink: &Sink, catalog: &Catalog) -> Value {
    let (key, label) = match sink {
        Sink::Paste => ("paste".to_owned(), catalog.t("gui.route.paste").to_owned()),
        Sink::Clipboard => (
            "clipboard".to_owned(),
            catalog.t("gui.route.clipboard").to_owned(),
        ),
        Sink::Overlay => (
            "overlay".to_owned(),
            catalog.t("gui.route.overlay").to_owned(),
        ),
        Sink::Client { name } => (
            format!("client:{name}"),
            catalog.tf("gui.route.back_to", &[("name", name)]),
        ),
        Sink::Subscribers { count } => (
            "subscribers".to_owned(),
            catalog.tf("gui.route.subscribers", &[("count", &count.to_string())]),
        ),
    };
    json!({ "key": key, "label": label })
}

/// The log lines of one message, each `{seq, row, at, session, from, to,
/// kind, filter, label, detail, level, activity}`: `filter` is `text`,
/// `audio`, `error` or `other`, and `activity` that the Activity page has
/// the line. A final text is one line per place it went.
fn rows(e: &RouteEvent, engine: &str, catalog: &Catalog) -> Vec<Value> {
    let from = origin(&e.origin, catalog);
    let source = from["label"].as_str().unwrap_or_default().to_owned();
    let anya = catalog.t("gui.route.anya").to_owned();
    let back = match &e.origin {
        Origin::Remote { .. } => source.clone(),
        _ => anya.clone(),
    };
    let local = matches!(e.origin, Origin::Dictation { .. });
    let line = |row: usize,
                from: &str,
                to: &str,
                kind: &str,
                filter: &str,
                detail: String,
                level: &str,
                activity: bool| {
        json!({
            "seq": e.seq, "row": row, "at": e.at, "session": e.session,
            "from": from, "to": to, "kind": kind, "filter": filter,
            "label": catalog.t(match kind {
                "start" => "gui.route.step_start",
                "audio" => "gui.route.step_audio",
                "partial" => "gui.route.step_partial",
                "final" => "gui.route.step_final",
                "failed" => "gui.route.step_failed",
                _ => "gui.route.step_ended",
            }),
            "detail": detail, "level": level, "activity": activity,
        })
    };
    match &e.step {
        Step::Start { streaming } => {
            let how = catalog.t(if *streaming {
                "gui.route.streaming"
            } else {
                "gui.route.batch"
            });
            let role = from["role"].as_str().unwrap_or_default();
            let detail = catalog.tf("gui.route.start_detail", &[("role", role), ("how", how)]);
            vec![line(0, &source, &anya, "start", "other", detail, "", false)]
        }
        Step::Audio { ms } => {
            let seconds = format!("{:.1}", *ms as f64 / 1000.0);
            let detail = catalog.tf("gui.route.seconds", &[("seconds", &seconds)]);
            vec![line(
                0, &source, engine, "audio", "audio", detail, "", false,
            )]
        }
        Step::Partial { text } => {
            vec![line(
                0,
                engine,
                &back,
                "partial",
                "text",
                text.clone(),
                "",
                false,
            )]
        }
        Step::Final { text, sinks } if sinks.is_empty() => {
            vec![line(
                0,
                engine,
                &anya,
                "final",
                "text",
                text.clone(),
                "ok",
                local,
            )]
        }
        Step::Final { text, sinks } => sinks
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let to = sink(s, catalog)["label"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let detail = match s {
                    Sink::Subscribers { count } => {
                        catalog.tf("gui.route.granted", &[("count", &count.to_string())])
                    }
                    _ => text.clone(),
                };
                line(
                    i,
                    engine,
                    &to,
                    "final",
                    "text",
                    detail,
                    "ok",
                    local && i == 0,
                )
            })
            .collect(),
        Step::Failed { error, detail } => {
            let text = catalog.error(*error, detail.as_deref());
            vec![line(
                0, engine, &back, "failed", "error", text, "error", false,
            )]
        }
        Step::Ended { reason } => {
            let key = match reason {
                EndReason::Cancelled => "gui.route.ended_cancelled",
                EndReason::Closed => "gui.route.ended_closed",
                _ => "gui.route.ended_other",
            };
            vec![line(
                0,
                &source,
                &anya,
                "ended",
                "other",
                catalog.t(key).to_owned(),
                "",
                false,
            )]
        }
    }
}

/// One activity line: `text` to show, `level` for its dot, `note` the place a
/// final text went, `engine` the title of the engine that recognized it,
/// `session` for the route page to point at it, `detail` the untranslated
/// specifics of a failure.
fn entry(entry: &Entry, engines: &[EngineInfo], catalog: &Catalog) -> Value {
    match entry {
        Entry::Final {
            at,
            source,
            text,
            delivered,
            session,
            engine,
        } => {
            let note = match delivered {
                Delivered::Pasted => catalog.t("gui.delivered.pasted"),
                Delivered::Copied => catalog.t("gui.delivered.copied"),
                Delivered::Kept => catalog.t("gui.delivered.kept"),
            };
            let engine = engine
                .as_deref()
                .map(|id| engine_title(engines, id, catalog));
            json!({
                "at": at, "kind": "final", "source": source, "level": "ok",
                "text": text, "note": note, "engine": engine, "session": session,
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
