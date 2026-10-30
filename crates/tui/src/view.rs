//! Drawing the TUI from its state.

use anya_config::DEFAULT_ADDRESS;
use anya_core::audio::DeviceKind;
use anya_protocol::{FieldKind, Manifest, Source};
use anya_runtime::{
    DebugEvent, Delivered, Dictation, Entry, Lan, Notice, PluginLoad, Subscription,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Group, Level, LineEdit, Screen, SettingEdit, choices};
use crate::form::{FIELDS, Field, Kind};
use anya_config::EngineRole;

const LOG_LINES: usize = 5;
const LOG_LINES_EXPANDED: usize = 100;
const MODAL_WIDTH: u16 = 84;

fn title() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}
fn label() -> Style {
    Style::new().fg(Color::Yellow)
}
fn value() -> Style {
    Style::new().fg(Color::White)
}
fn ok() -> Style {
    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
}
fn warn() -> Style {
    Style::new()
        .fg(Color::LightYellow)
        .add_modifier(Modifier::BOLD)
}
fn error() -> Style {
    Style::new()
        .fg(Color::LightRed)
        .add_modifier(Modifier::BOLD)
}
fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// Draws the whole screen.
pub(crate) fn draw(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    // Title, stats, settings header, status header and its three lines, log
    // header, footer.
    let fixed = 9usize;
    let log_rows = if app.log_expanded {
        usize::from(area.height)
            .saturating_sub(fixed + 8)
            .clamp(LOG_LINES, LOG_LINES_EXPANDED)
    } else {
        LOG_LINES
    };
    let [
        head,
        stats,
        form_head,
        form,
        status_head,
        status,
        log_head,
        log,
        foot,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(u16::try_from(log_rows).unwrap_or(u16::MAX)),
        Constraint::Length(1),
    ])
    .areas(area);

    let c = &app.catalog;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(c.t("common.app_name"), title()),
            Span::raw("  "),
            Span::styled(c.t("tui.tagline"), dim()),
        ])),
        head,
    );
    frame.render_widget(Paragraph::new(stats_line(app)), stats);
    frame.render_widget(
        Paragraph::new(section(c.t("tui.section.settings"))),
        form_head,
    );
    draw_form(app, frame, form);
    let c = &app.catalog;
    frame.render_widget(
        Paragraph::new(section(c.t("tui.section.status"))),
        status_head,
    );
    frame.render_widget(Paragraph::new(status_lines(app)), status);
    let log_title = if app.log_expanded {
        "tui.section.log_expanded"
    } else {
        "tui.section.log"
    };
    frame.render_widget(Paragraph::new(section(c.t(log_title))), log_head);
    frame.render_widget(Paragraph::new(log_lines(app, log_rows)), log);
    frame.render_widget(Paragraph::new(footer(app)), foot);

    match app.screen.clone() {
        Screen::Picker(role, at) => draw_picker(app, frame, role, at),
        Screen::Settings(state) => {
            draw_settings(app, frame, &state.engine, state.cursor, state.edit)
        }
        _ => {}
    }
}

fn section(name: &str) -> Line<'static> {
    Line::styled(format!("── {name} ──"), dim())
}

/// Characters per minute of audio, 0 without audio.
fn per_minute(chars: u64, audio_ms: u64) -> u64 {
    (chars * 60_000 + audio_ms / 2)
        .checked_div(audio_ms)
        .unwrap_or(0)
}

fn stats_line(app: &App) -> Line<'static> {
    let s = app.status.stats;
    let text = app.catalog.tf(
        "tui.stats",
        &[
            ("sessions", &s.sessions.to_string()),
            ("chars", &s.chars.to_string()),
            ("average", &per_minute(s.chars, s.audio_ms).to_string()),
            (
                "latest",
                &per_minute(s.last_chars, s.last_audio_ms).to_string(),
            ),
        ],
    );
    Line::styled(text, value())
}

/// `text` cut into lines of at most `width` cells, at spaces where it can.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > width && !line.is_empty() {
            match line.rfind(' ').filter(|i| *i > 0) {
                Some(i) => {
                    let rest = line.split_off(i + 1);
                    out.push(line.trim_end().to_owned());
                    used = rest.chars().map(|c| c.width().unwrap_or(0)).sum();
                    line = rest;
                }
                None => {
                    out.push(std::mem::take(&mut line));
                    used = 0;
                }
            }
        }
        line.push(ch);
        used += w;
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// Indented, styled lines of `text` wrapped to `width`.
fn sub_lines(text: &str, width: usize, indent: &str, style: Style) -> Vec<Line<'static>> {
    wrap(text, width.saturating_sub(indent.len()))
        .into_iter()
        .map(|l| Line::styled(format!("{indent}{l}"), style))
        .collect()
}

/// The text being typed, the cursor shown as a reversed cell.
fn edit_spans(edit: &LineEdit, masked: bool) -> Vec<Span<'static>> {
    let shown: Vec<char> = if masked {
        vec!['•'; edit.chars.len()]
    } else {
        edit.chars.clone()
    };
    let style = Style::new().fg(Color::Green);
    let before: String = shown[..edit.cursor].iter().collect();
    let at = shown
        .get(edit.cursor)
        .map_or(" ".to_owned(), |c| c.to_string());
    let after: String = shown.get(edit.cursor + 1..).unwrap_or(&[]).iter().collect();
    vec![
        Span::styled(before, style),
        Span::styled(at, style.add_modifier(Modifier::REVERSED)),
        Span::styled(after, style),
    ]
}

fn tag(app: &App, manifest: &Manifest) -> Span<'static> {
    if manifest.streaming {
        Span::styled(app.catalog.t("tui.engine.streaming").to_owned(), ok())
    } else {
        Span::styled(app.catalog.t("tui.engine.batch").to_owned(), dim())
    }
}

/// The value part of a field's line.
fn value_spans(app: &App, field: Field, editing: bool) -> Vec<Span<'static>> {
    let c = &app.catalog;
    match field.kind() {
        Kind::Switch => {
            let on = match field {
                Field::ContinuousEnabled => app.status.continuous_on,
                _ => field.get(&app.draft) == "true",
            };
            if on {
                vec![Span::styled(c.t("tui.switch.on").to_owned(), ok())]
            } else {
                vec![Span::styled(c.t("tui.switch.off").to_owned(), dim())]
            }
        }
        Kind::Select => {
            let options = app.options(field);
            let at = app.selected(field, &options);
            let text = options.get(at).map(|o| o.label.clone()).unwrap_or_default();
            if editing {
                vec![Span::styled(format!("[{text} ▲▼]"), ok())]
            } else {
                vec![Span::styled(text, value())]
            }
        }
        Kind::Text => match &app.screen {
            Screen::Text(edit) if editing => edit_spans(edit, false),
            _ => {
                let text = app.shown(field);
                if !text.trim().is_empty() {
                    vec![Span::styled(text, value())]
                } else if field == Field::Address {
                    vec![Span::styled(
                        c.tf("tui.value.default_address", &[("address", DEFAULT_ADDRESS)]),
                        dim(),
                    )]
                } else {
                    vec![Span::styled(c.t("tui.value.not_set").to_owned(), dim())]
                }
            }
        },
        Kind::Action => {
            let Some(role) = field.role() else {
                return vec![Span::styled(
                    c.t("tui.engine.settings_hint").to_owned(),
                    dim(),
                )];
            };
            let id = app.draft.recognition.engine_id(role);
            let Some(engine) = app.engine(id) else {
                return vec![Span::styled(
                    c.tf("tui.engine.unknown", &[("id", id)]),
                    error(),
                )];
            };
            let state = if engine.manifest.fields.is_empty() {
                Span::styled(c.t("tui.engine.no_settings_short").to_owned(), dim())
            } else if app.ready(id) {
                Span::styled(c.t("tui.engine.ready_short").to_owned(), ok())
            } else {
                Span::styled(c.t("tui.engine.missing_short").to_owned(), warn())
            };
            vec![
                Span::styled(app.title(&engine.manifest), value()),
                Span::raw(" "),
                tag(app, &engine.manifest),
                Span::raw(" "),
                state,
            ]
        }
    }
}

/// The options of a device select being edited, grouped.
fn device_list(app: &App, field: Field) -> Vec<Line<'static>> {
    let c = &app.catalog;
    let options = app.options(field);
    let at = app.selected(field, &options);
    let mut out = Vec::new();
    let mut group = None;
    for (i, o) in options.iter().enumerate() {
        if group != Some(o.group) {
            group = Some(o.group);
            let head = match o.group {
                Group::Mic => Some("tui.device.microphones"),
                Group::Loopback => Some("tui.device.loopbacks"),
                Group::Plain | Group::Missing => None,
            };
            if let Some(key) = head {
                out.push(Line::styled(format!("      {}", c.t(key)), dim()));
            }
        }
        let (mark, style) = if i == at {
            ("●", ok())
        } else {
            ("○", value())
        };
        out.push(Line::styled(format!("        {mark} {}", o.label), style));
    }
    out
}

fn draw_form(app: &mut App, frame: &mut Frame, area: Rect) {
    let width = usize::from(area.width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor_range = (0, 0);
    let c = app.catalog.clone();
    for (i, &field) in FIELDS.iter().enumerate() {
        let start = lines.len();
        if let Some(group) = field.group() {
            lines.push(Line::styled(
                format!("  {}", c.t(group)),
                Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ));
        }
        let here = app.cursor == Some(i);
        let editing = here && matches!(app.screen, Screen::Text(_) | Screen::Select);
        let marker = match (here, editing) {
            (true, true) => "▶ ",
            (true, false) => "▸ ",
            _ => "  ",
        };
        let (label_key, help_key) = field.text();
        let mut spans = vec![
            Span::styled(marker, ok()),
            Span::styled(format!("{}: ", c.t(label_key)), label()),
        ];
        spans.extend(value_spans(app, field, editing));
        lines.push(Line::from(spans));

        if let Some(msg) = app.errors.get(&field) {
            lines.extend(sub_lines(&msg.render(&c), width, "    ", error()));
        }
        if editing
            && app.screen == Screen::Select
            && matches!(field, Field::DictationDevice | Field::ContinuousDevice)
        {
            lines.extend(device_list(app, field));
        }
        if field == Field::Address {
            let line = match &app.status.subscription {
                Subscription::Listening { url, subscribers } => Line::styled(
                    format!(
                        "    {}",
                        c.tf(
                            "tui.subscription.listening",
                            &[("url", url), ("count", &subscribers.to_string())],
                        )
                    ),
                    Style::new().fg(Color::Green),
                ),
                Subscription::Down { detail } => Line::styled(
                    format!(
                        "    {}",
                        c.tf("tui.subscription.down", &[("detail", detail)])
                    ),
                    warn(),
                ),
            };
            lines.push(line);
        }
        if field == Field::LanPort {
            let line = match &app.status.lan {
                Lan::Off => None,
                Lan::Listening { url, fingerprint } => Some(Line::styled(
                    format!(
                        "    {}",
                        c.tf(
                            "tui.lan.listening",
                            &[("url", url), ("fingerprint", fingerprint)],
                        )
                    ),
                    Style::new().fg(Color::Green),
                )),
                Lan::Down { detail } => Some(Line::styled(
                    format!("    {}", c.tf("tui.lan.down", &[("detail", detail)])),
                    warn(),
                )),
            };
            lines.extend(line);
        }
        if cfg!(target_os = "macos")
            && field == Field::ContinuousDevice
            && (here || app.help)
            && matches!(app.devices, Some(Ok(_)))
            && !app.listed().iter().any(|d| d.kind == DeviceKind::Loopback)
        {
            lines.extend(sub_lines(
                c.t("tui.device.no_loopback"),
                width,
                "    ",
                dim(),
            ));
        }
        if app.help {
            lines.extend(sub_lines(c.t(help_key), width, "    ", dim()));
        }
        if here {
            cursor_range = (start, lines.len());
        }
    }

    let height = usize::from(area.height);
    if app.cursor.is_some() {
        let (start, end) = cursor_range;
        if start < app.scroll {
            app.scroll = start;
        } else if end > app.scroll + height {
            app.scroll = end.saturating_sub(height);
        }
    }
    app.scroll = app.scroll.min(lines.len().saturating_sub(height));
    let shown: Vec<Line<'static>> = lines.into_iter().skip(app.scroll).collect();
    frame.render_widget(Paragraph::new(shown), area);
}

fn status_lines(app: &App) -> Vec<Line<'static>> {
    let c = &app.catalog;
    let s = &app.status;
    let background = |spans: &mut Vec<Span<'static>>| {
        if s.finishing > 0 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                c.tf(
                    "tui.status.background",
                    &[("count", &s.finishing.to_string())],
                ),
                dim(),
            ));
        }
    };
    let mut spans = Vec::new();
    match &s.dictation {
        Dictation::Idle if s.finishing > 0 => {
            spans.push(Span::styled(c.t("tui.status.finishing").to_owned(), warn()));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                c.t("tui.status.finishing_detail").to_owned(),
                value(),
            ));
        }
        Dictation::Idle => {
            let detail = if app.base.dictation.enabled {
                "tui.status.idle_detail"
            } else {
                "tui.status.idle_off"
            };
            spans.push(Span::styled(c.t("tui.status.idle").to_owned(), dim()));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(c.t(detail).to_owned(), value()));
        }
        Dictation::Recording => {
            spans.push(Span::styled(c.t("tui.status.recording").to_owned(), ok()));
            background(&mut spans);
        }
        Dictation::Stopping => {
            spans.push(Span::styled(c.t("tui.status.stopping").to_owned(), warn()));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                c.t("tui.status.stopping_detail").to_owned(),
                value(),
            ));
            background(&mut spans);
        }
        Dictation::Failed {
            error: code,
            detail,
        } => {
            spans.push(Span::styled(c.t("tui.status.failed").to_owned(), error()));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(c.error(*code, detail.as_deref()), error()));
            background(&mut spans);
        }
    }
    let live = match s.live_text.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Line::styled(
            c.tf("tui.status.live", &[("text", text)]),
            Style::new().fg(Color::White).add_modifier(Modifier::ITALIC),
        ),
        _ => Line::raw(""),
    };
    let continuous = if s.continuous_on {
        Line::styled(
            c.t("tui.status.continuous_on").to_owned(),
            Style::new().fg(Color::Green),
        )
    } else {
        Line::styled(c.t("tui.status.continuous_off").to_owned(), dim())
    };
    let mut continuous = continuous;
    if !app.plugins.is_empty() {
        continuous.spans.push(Span::raw("  "));
        continuous
            .spans
            .push(Span::styled(plugins_text(app), dim()));
    }
    vec![Line::from(spans), live, continuous]
}

/// `Plugins: echo (ready), other (off)`, each in its own words.
fn plugins_text(app: &App) -> String {
    let c = &app.catalog;
    let list: Vec<String> = app
        .plugins
        .iter()
        .map(|p| {
            let state = match &p.load {
                PluginLoad::HostDown => c.t("tui.plugins.host_down").to_owned(),
                PluginLoad::Off => c.t("tui.plugins.off").to_owned(),
                PluginLoad::Loading => c.t("tui.plugins.loading").to_owned(),
                PluginLoad::Ready => c.t("tui.plugins.ready").to_owned(),
                PluginLoad::Failed { detail } => c.tf("tui.plugins.failed", &[("detail", detail)]),
            };
            c.tf("tui.plugins.item", &[("id", &p.id), ("state", &state)])
        })
        .collect();
    c.tf("tui.plugins.line", &[("list", &list.join(", "))])
}

fn source_label(app: &App, source: Source) -> String {
    let key = match source {
        Source::Dictation => "common.dictation",
        Source::Continuous => "common.continuous",
        Source::Unknown => "tui.source.unknown",
    };
    app.catalog.t(key).to_owned()
}

fn notice_key(notice: Notice) -> &'static str {
    match notice {
        Notice::ContinuousOn => "tui.notice.continuous_on",
        Notice::ContinuousOff => "tui.notice.continuous_off",
        Notice::DictationOn => "tui.notice.dictation_on",
        Notice::DictationOff => "tui.notice.dictation_off",
        Notice::ConfigReloaded => "tui.notice.config_reloaded",
        Notice::ConfigBroken => "tui.notice.config_broken",
        Notice::HotwordsUnreadable => "tui.notice.hotwords_unreadable",
        Notice::DeliveryFailed => "tui.notice.delivery_failed",
        Notice::PluginsUnavailable => "tui.notice.plugins_unavailable",
        Notice::OverlayUnavailable => "tui.notice.overlay_unavailable",
    }
}

fn entry_line(app: &App, entry: &Entry) -> (u64, String, Style) {
    let c = &app.catalog;
    match entry {
        Entry::Final {
            at,
            source,
            text,
            delivered,
        } => {
            let delivered = match delivered {
                Delivered::Pasted => "tui.delivered.pasted",
                Delivered::Copied => "tui.delivered.copied",
                Delivered::Kept => "tui.delivered.kept",
            };
            let line = c.tf(
                "tui.log.final",
                &[
                    ("source", &source_label(app, *source)),
                    ("delivered", c.t(delivered)),
                    ("text", text),
                ],
            );
            (*at, line, value())
        }
        Entry::Failed {
            at,
            source,
            error: code,
            detail,
        } => {
            let line = c.tf(
                "tui.log.failed",
                &[
                    ("source", &source_label(app, *source)),
                    ("error", &c.error(*code, detail.as_deref())),
                ],
            );
            (*at, line, Style::new().fg(Color::LightRed))
        }
        Entry::Debug { at, event } => {
            let line = match event {
                DebugEvent::KeyDown { combo } => c.tf("tui.debug.key_down", &[("combo", combo)]),
                DebugEvent::KeyUp { combo } => c.tf("tui.debug.key_up", &[("combo", combo)]),
                DebugEvent::Timing {
                    connect_ms,
                    first_text_ms,
                    final_ms,
                } => [
                    ("tui.debug.connect", connect_ms),
                    ("tui.debug.first_text", first_text_ms),
                    ("tui.debug.final", final_ms),
                ]
                .into_iter()
                .filter_map(|(key, ms)| Some(c.tf(key, &[("ms", &(*ms)?.to_string())])))
                .collect::<Vec<_>>()
                .join(" · "),
            };
            (*at, c.tf("tui.log.debug", &[("text", &line)]), dim())
        }
        Entry::Notice { at, notice, detail } => {
            let text = c.t(notice_key(*notice));
            let line = match detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                Some(d) => c.tf("tui.log.notice_detail", &[("notice", text), ("detail", d)]),
                None => text.to_owned(),
            };
            let style = match notice {
                Notice::ConfigBroken
                | Notice::HotwordsUnreadable
                | Notice::DeliveryFailed
                | Notice::PluginsUnavailable
                | Notice::OverlayUnavailable => Style::new().fg(Color::LightYellow),
                _ => dim(),
            };
            (*at, line, style)
        }
    }
}

fn log_lines(app: &App, rows: usize) -> Vec<Line<'static>> {
    let c = &app.catalog;
    let mut all: Vec<(u64, String, Style)> =
        app.activity.iter().map(|e| entry_line(app, e)).collect();
    for note in &app.notes {
        let msg = note.msg.render(c);
        let text = match note.field {
            Some(f) => c.tf(
                "tui.log.field",
                &[("field", c.t(f.text().0)), ("message", &msg)],
            ),
            None => msg,
        };
        let style = match note.level {
            Level::Info => dim(),
            Level::Ok => Style::new().fg(Color::Green),
            Level::Warn => Style::new().fg(Color::LightYellow),
            Level::Error => Style::new().fg(Color::LightRed),
        };
        all.push((note.at, text, style));
    }
    all.sort_by_key(|(at, _, _)| *at);
    let skip = all.len().saturating_sub(rows);
    all.into_iter()
        .skip(skip)
        .map(|(at, text, style)| {
            Line::from(vec![
                Span::styled(format!("  {}  ", clock(app, at)), dim()),
                Span::styled(text, style),
            ])
        })
        .collect()
}

/// `at` (Unix ms) as local `HH:MM:SS`.
fn clock(app: &App, at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        .map(|t| t.to_zoned(app.tz.clone()).strftime("%H:%M:%S").to_string())
        .unwrap_or_default()
}

fn footer(app: &App) -> Line<'static> {
    let c = &app.catalog;
    let hint = match &app.screen {
        Screen::Nav => "tui.hint.nav",
        Screen::Text(_) => "tui.hint.text",
        Screen::Select => "tui.hint.select",
        Screen::Recording(_) => "tui.hint.recording",
        Screen::Picker(..) => "tui.hint.picker",
        Screen::Settings(s) if s.edit.is_some() => "tui.hint.settings_edit",
        Screen::Settings(_) => "tui.hint.settings",
    };
    let mut spans = Vec::new();
    if app.dirty() {
        spans.push(Span::styled(c.t("tui.unsaved").to_owned(), warn()));
        spans.push(Span::raw("  "));
    }
    let style = if matches!(app.screen, Screen::Recording(_)) {
        warn()
    } else {
        dim()
    };
    spans.push(Span::styled(c.t(hint).to_owned(), style));
    Line::from(spans)
}

/// A centered box for a modal with `rows` lines of content.
fn modal_area(area: Rect, rows: usize) -> Rect {
    let width = MODAL_WIDTH
        .min(area.width.saturating_sub(4))
        .max(area.width.min(20));
    let height = u16::try_from(rows + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(area.height.min(3));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn draw_modal(frame: &mut Frame, rect: Rect, heading: Line<'static>, lines: Vec<Line<'static>>) {
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(Color::Cyan))
                .title(heading),
        ),
        rect,
    );
}

fn draw_picker(app: &App, frame: &mut Frame, role: Option<EngineRole>, at: usize) {
    let c = &app.catalog;
    let recognition = &app.draft.recognition;
    let in_use = |id: &str| match role {
        Some(role) => recognition.engine_id(role) == id,
        None => recognition.engines_in_use().contains(&id),
    };
    let mut lines = Vec::new();
    for (i, &index) in app.picker_order().iter().enumerate() {
        let m = &app.engines[index].manifest;
        let state = if app.ready(&m.id) {
            Span::styled(
                c.t("tui.picker.ready").to_owned(),
                Style::new().fg(Color::Green),
            )
        } else {
            Span::styled(c.t("tui.picker.not_ready").to_owned(), dim())
        };
        lines.push(Line::from(vec![
            Span::styled(if i == at { "▸ " } else { "  " }, ok()),
            Span::styled(if in_use(&m.id) { "● " } else { "  " }, ok()),
            Span::styled(app.title(m), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            tag(app, m),
            Span::raw("  "),
            state,
        ]));
        let blurb = m.blurb.get(c.locale());
        if !blurb.is_empty() {
            lines.extend(sub_lines(
                blurb,
                usize::from(MODAL_WIDTH) - 2,
                "      ",
                dim(),
            ));
        }
    }
    let rect = modal_area(frame.area(), lines.len());
    let heading = match role {
        Some(role) => c.tf(
            "tui.picker.title_for",
            &[("use", c.t(&format!("tui.use.{}", role.key())))],
        ),
        None => c.t("tui.picker.title_settings").to_owned(),
    };
    let heading = Line::styled(format!(" {heading} "), title());
    draw_modal(frame, rect, heading, lines);
}

/// A credential as shown when it is not being typed: never in full.
pub(crate) fn mask(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() >= 12 {
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("••••••••{tail}")
    } else {
        "••••••••".to_owned()
    }
}

fn draw_settings(
    app: &mut App,
    frame: &mut Frame,
    id: &str,
    cursor: usize,
    edit: Option<SettingEdit>,
) {
    let Some(engine) = app.engine(id).cloned() else {
        return;
    };
    let c = app.catalog.clone();
    let m = &engine.manifest;
    let settings = app.draft.recognition.settings(id);
    let inner =
        usize::from(MODAL_WIDTH.min(frame.area().width.saturating_sub(4))).saturating_sub(2);
    let mut lines = Vec::new();
    let blurb = m.blurb.get(c.locale());
    if !blurb.is_empty() {
        lines.extend(sub_lines(blurb, inner, "", dim()));
        lines.push(Line::raw(""));
    }
    let mut range = (0, 0);
    for (i, field) in m.fields.iter().enumerate() {
        let start = lines.len();
        let here = i == cursor;
        let current = settings.get(&field.key).cloned().unwrap_or_default();
        let editing = if here { edit.as_ref() } else { None };
        let marker = match (here, editing.is_some()) {
            (true, true) => "▶ ",
            (true, false) => "▸ ",
            _ => "  ",
        };
        let required = if field.required { "*" } else { "" };
        let mut spans = vec![
            Span::styled(marker, ok()),
            Span::styled(
                format!("{}{required}: ", field.label.get(c.locale())),
                label(),
            ),
        ];
        match (editing, &field.kind) {
            (Some(SettingEdit::Text(e)), kind) => {
                spans.extend(edit_spans(e, *kind == FieldKind::Secret));
            }
            (Some(SettingEdit::Choice(at)), kind) => {
                let list = choices(kind, &current);
                let v = list.get(*at).cloned().unwrap_or_default();
                let text = if v.is_empty() {
                    c.t("tui.settings.default").to_owned()
                } else {
                    v
                };
                spans.push(Span::styled(format!("[{text} ▲▼]"), ok()));
            }
            (None, FieldKind::Choice { .. }) if current.is_empty() => {
                spans.push(Span::styled(c.t("tui.settings.default").to_owned(), dim()));
            }
            (None, _) if current.is_empty() => {
                spans.push(Span::styled(c.t("tui.settings.empty").to_owned(), dim()));
            }
            (None, FieldKind::Secret) => spans.push(Span::styled(mask(&current), value())),
            (None, _) => spans.push(Span::styled(current, value())),
        }
        lines.push(Line::from(spans));
        let help = field.help.get(c.locale());
        if !help.is_empty() {
            lines.extend(sub_lines(help, inner, "    ", dim()));
        }
        if here {
            range = (start, lines.len());
        }
    }
    let rect = modal_area(frame.area(), lines.len());
    let height = usize::from(rect.height.saturating_sub(2));
    if range.0 < app.settings_scroll {
        app.settings_scroll = range.0;
    } else if range.1 > app.settings_scroll + height {
        app.settings_scroll = range.1.saturating_sub(height);
    }
    let shown: Vec<Line<'static>> = lines.into_iter().skip(app.settings_scroll).collect();
    let mut heading = vec![
        Span::styled(
            format!(
                " {} ",
                c.tf("tui.settings.title", &[("engine", &app.title(m))])
            ),
            title(),
        ),
        tag(app, m),
        Span::raw(" "),
    ];
    if app.ready(id) {
        heading.push(Span::styled(
            c.t("tui.picker.ready").to_owned(),
            Style::new().fg(Color::Green),
        ));
        heading.push(Span::raw(" "));
    }
    draw_modal(frame, rect, Line::from(heading), shown);
}
