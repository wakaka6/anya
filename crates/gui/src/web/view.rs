//! What `/api/state` and `/api/status` answer: the runtime's data, shaped
//! for the page and with every word already translated.

use anya_config::{Config, DEFAULT_ADDRESS, LANGUAGES, POSITIONS};
use anya_core::audio::DeviceKind;
use anya_i18n::{Catalog, LOCALES};
use anya_protocol::{FieldKind, Manifest, Source};
use anya_runtime::{Delivered, Dictation, Entry, HotkeyRole, Notice, Subscription};
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
        "config": config,
    })
}

/// `config` as the page edits it: hotkeys in canonical names (so an
/// unchanged list compares equal), the engine id resolved.
fn canonical(page: &Page, mut config: Config) -> Config {
    let control = &page.control;
    if let Ok(value) = control.check_hotkeys(&config.dictation.hotkeys, HotkeyRole::Dictation) {
        config.dictation.hotkeys = value;
    }
    if let Ok(value) = control.check_hotkeys(&config.continuous.hotkey, HotkeyRole::Continuous) {
        config.continuous.hotkey = value;
    }
    config.recognition.engine = config.recognition.engine_id().to_owned();
    config
}

fn engines(page: &Page, config: &Config, catalog: &Catalog) -> Vec<Value> {
    let locale = catalog.locale();
    let mut out: Vec<Value> = page
        .control
        .engines()
        .into_iter()
        .map(|e| engine(&e.manifest, e.ready, locale))
        .collect();
    // An engine the config names but Anya lacks stays visible, so the user
    // sees what is chosen; saving it is refused with the reason.
    let current = config.recognition.engine_id();
    if !out.iter().any(|e| e["id"] == current) {
        out.push(json!({
            "id": current,
            "title": current,
            "blurb": catalog.t("gui.engine.custom"),
            "docs": null,
            "streaming": false,
            "ready": false,
            "custom": true,
            "fields": [],
        }));
    }
    out
}

fn engine(m: &Manifest, ready: bool, locale: &str) -> Value {
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
    let mut out = vec![option("system", catalog.t("gui.ui_language.system"))];
    out.extend(LOCALES.iter().map(|tag| {
        let key = match *tag {
            "zh-CN" => Some("gui.ui_language.zh_cn"),
            "en" => Some("gui.ui_language.en"),
            _ => None,
        };
        option(tag, key.map_or(*tag, |k| catalog.t(k)))
    }));
    let current = current.trim();
    if current != "system" && !current.is_empty() && !LOCALES.contains(&current) {
        out.push(option(current, current));
    }
    out
}

/// The live state, polled a few times a second.
pub(super) fn status(page: &Page, catalog: &Catalog) -> Value {
    let status = page.control.status();
    let enabled = page.control.config().dictation.enabled;
    let (pill, label) = match &status.dictation {
        Dictation::Recording => ("recording", "gui.state.recording"),
        Dictation::Stopping => ("stopping", "gui.state.stopping"),
        Dictation::Failed { .. } => ("failed", "gui.state.failed"),
        Dictation::Idle if status.finishing > 0 => ("finishing", "gui.state.finishing"),
        Dictation::Idle if !enabled => ("off", "gui.state.off"),
        Dictation::Idle => ("idle", "gui.state.idle"),
    };
    let mut detail = Vec::new();
    if let Dictation::Failed { error, detail: d } = &status.dictation {
        detail.push(catalog.error(*error, d.as_deref()));
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
    let activity = page.control.activity();
    let skip = activity.len().saturating_sub(ACTIVITY_SHOWN);
    let activity: Vec<Value> = activity[skip..].iter().map(|e| entry(e, catalog)).collect();
    json!({
        "pill": { "state": pill, "label": catalog.t(label) },
        "detail": detail.join("  "),
        "subscription": subscription,
        "stats": status.stats,
        "activity": activity,
        "status": status,
    })
}

fn source_label(source: Source, catalog: &Catalog) -> &str {
    match source {
        Source::Dictation => catalog.t("common.dictation"),
        Source::Continuous => catalog.t("common.continuous"),
        Source::Unknown => "",
    }
}

/// One activity line: `text` to show, `level` for its dot, `note` beside it.
fn entry(entry: &Entry, catalog: &Catalog) -> Value {
    match entry {
        Entry::Final {
            at,
            source,
            text,
            delivered,
        } => {
            let note = match (source, delivered) {
                (Source::Dictation, Delivered::Pasted) => catalog.t("gui.delivered.pasted"),
                (Source::Dictation, Delivered::Copied) => catalog.t("gui.delivered.copied"),
                (Source::Dictation, Delivered::Kept) => catalog.t("gui.delivered.kept"),
                (source, _) => source_label(*source, catalog),
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
            "text": catalog.error(*error, detail.as_deref()),
            "note": source_label(*source, catalog),
        }),
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
