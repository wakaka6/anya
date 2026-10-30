//! The TUI's state and what each key does to it. Drawing is in `view`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anya_config::{Config, EngineRole, LANGUAGES, POSITIONS};
use anya_core::audio::{Device, DeviceKind};
use anya_i18n::{Catalog, LOCALES};
use anya_protocol::{FieldKind, Manifest};
use anya_runtime::{
    Control, EngineInfo, Entry, HotkeyRole, Info, InstalledPlugin, Invalid, SaveError, Saved,
    Status,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::form::{
    FIELDS, Field, Kind, Msg, language_key, mode_key, output_key, position_key, split_hotkeys,
    ui_language_key,
};

/// Opens a URL in the system browser; the error is untranslated.
pub(crate) type Opener = Box<dyn Fn(&str) -> Result<(), String>>;

/// The catalog key of a select option's label, if it has one.
type LabelKey = fn(&str) -> Option<&'static str>;

const STATUS_EVERY: Duration = Duration::from_millis(200);
const ACTIVITY_EVERY: Duration = Duration::from_millis(500);
const DEVICES_EVERY: Duration = Duration::from_secs(5);
/// How long the OS waits for a hotkey to record.
const RECORD_FOR: Duration = Duration::from_secs(15);
const NOTES_KEPT: usize = 100;

/// How a note in the log reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

/// A line the TUI itself adds to the log.
#[derive(Debug, Clone)]
pub(crate) struct Note {
    pub(crate) at: u64,
    pub(crate) level: Level,
    /// The field it is about, shown before the message.
    pub(crate) field: Option<Field>,
    pub(crate) msg: Msg,
}

/// One line of text being typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LineEdit {
    pub(crate) chars: Vec<char>,
    pub(crate) cursor: usize,
}

impl LineEdit {
    pub(crate) fn new(text: &str) -> LineEdit {
        let chars: Vec<char> = text.chars().collect();
        LineEdit {
            cursor: chars.len(),
            chars,
        }
    }

    pub(crate) fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                self.chars.clear();
                self.cursor = 0;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.chars.len(),
            KeyCode::Char(c) if !ctrl => {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            _ => {}
        }
    }
}

/// An engine setting being changed in the engine modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingEdit {
    Text(LineEdit),
    /// An index into [`choices`]; 0 is the engine's default.
    Choice(usize),
}

/// The engine settings modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsState {
    pub(crate) engine: String,
    pub(crate) cursor: usize,
    pub(crate) edit: Option<SettingEdit>,
}

/// What has the keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Screen {
    /// Moving between fields.
    Nav,
    /// Typing into the field under the cursor.
    Text(LineEdit),
    /// Choosing an option of the select under the cursor.
    Select,
    /// The OS is recording a hotkey for this field.
    Recording(Field),
    /// The engine picker: the use it chooses for (none: any engine, to set
    /// it up) and its cursor over [`App::picker_order`].
    Picker(Option<EngineRole>, usize),
    /// An engine's settings.
    Settings(SettingsState),
}

/// Where a select option belongs in the device list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Plain,
    Mic,
    Loopback,
    Missing,
}

/// One option of a select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opt {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) group: Group,
}

/// What background threads report.
enum Done {
    Devices(Result<Vec<Device>, String>),
    Recorded(Field, Result<Option<String>, String>),
}

/// The whole TUI.
pub(crate) struct App {
    control: Arc<dyn Control>,
    pub(crate) catalog: Catalog,
    opener: Opener,
    pub(crate) info: Info,
    /// The config in effect, as last read.
    pub(crate) base: Config,
    /// The config as the form shows it, unsaved edits included.
    pub(crate) draft: Config,
    /// Typed text that does not parse, shown instead of the draft's value.
    pub(crate) pending: BTreeMap<Field, String>,
    /// Why a field's value cannot be used.
    pub(crate) errors: BTreeMap<Field, Msg>,
    pub(crate) status: Status,
    /// Installed plugins and whether they loaded, polled with the status.
    pub(crate) plugins: Vec<InstalledPlugin>,
    pub(crate) activity: Vec<Entry>,
    pub(crate) engines: Vec<EngineInfo>,
    /// `None` until the first listing arrives.
    pub(crate) devices: Option<Result<Vec<Device>, String>>,
    pub(crate) devices_pending: bool,
    pub(crate) notes: Vec<Note>,
    pub(crate) screen: Screen,
    /// The field under the cursor; none at start.
    pub(crate) cursor: Option<usize>,
    /// First form line shown.
    pub(crate) scroll: usize,
    /// First engine-settings line shown.
    pub(crate) settings_scroll: usize,
    pub(crate) help: bool,
    pub(crate) log_expanded: bool,
    quit_armed: bool,
    done: bool,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    last_status: Instant,
    last_activity: Instant,
    last_devices: Instant,
    pub(crate) tz: jiff::tz::TimeZone,
}

/// Now, in Unix ms.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A device setting that means the system default.
fn is_default(value: &str) -> bool {
    let v = value.trim();
    v.is_empty() || v.eq_ignore_ascii_case("default")
}

/// `device` is the one a device setting names, by id or by name.
fn names(device: &Device, value: &str) -> bool {
    device.id == value || device.name.eq_ignore_ascii_case(value.trim())
}

/// The values a choice field's editor cycles through: the engine's default
/// (empty) first, then its options, then the current value if it is neither.
pub(crate) fn choices(kind: &FieldKind, current: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    if let FieldKind::Choice { options } = kind {
        out.extend(options.iter().cloned());
    }
    if !out.iter().any(|o| o == current) {
        out.push(current.to_owned());
    }
    out
}

impl App {
    pub(crate) fn new(control: Arc<dyn Control>, catalog: Catalog, opener: Opener) -> App {
        let (tx, rx) = channel();
        let base = control.config();
        let long_ago = Instant::now()
            .checked_sub(DEVICES_EVERY)
            .unwrap_or_else(Instant::now);
        let mut app = App {
            info: control.info(),
            draft: base.clone(),
            base,
            status: control.status(),
            plugins: control.plugins(),
            activity: control.activity(),
            engines: control.engines(),
            control,
            catalog,
            opener,
            pending: BTreeMap::new(),
            errors: BTreeMap::new(),
            devices: None,
            devices_pending: false,
            notes: Vec::new(),
            screen: Screen::Nav,
            cursor: None,
            scroll: 0,
            settings_scroll: 0,
            help: false,
            log_expanded: false,
            quit_armed: false,
            done: false,
            tx,
            rx,
            last_status: Instant::now(),
            last_activity: Instant::now(),
            last_devices: long_ago,
            tz: jiff::tz::TimeZone::system(),
        };
        app.fetch_devices();
        app.last_devices = Instant::now();
        app
    }

    /// The user asked to leave.
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Takes what background work finished and polls what is due.
    pub(crate) fn tick(&mut self) {
        while let Ok(done) = self.rx.try_recv() {
            self.finish(done);
        }
        if self.last_status.elapsed() >= STATUS_EVERY {
            self.last_status = Instant::now();
            self.status = self.control.status();
            self.plugins = self.control.plugins();
        }
        if self.last_activity.elapsed() >= ACTIVITY_EVERY {
            self.last_activity = Instant::now();
            self.activity = self.control.activity();
            let config = self.control.config();
            self.take_config(config);
        }
        if self.last_devices.elapsed() >= DEVICES_EVERY {
            self.last_devices = Instant::now();
            self.engines = self.control.engines();
            self.fetch_devices();
        }
    }

    /// Polls everything now.
    #[cfg(test)]
    pub(crate) fn refresh(&mut self) {
        let long_ago = Instant::now()
            .checked_sub(DEVICES_EVERY)
            .unwrap_or_else(Instant::now);
        self.last_status = long_ago;
        self.last_activity = long_ago;
        self.last_devices = long_ago;
        self.tick();
    }

    /// Waits until the device listing and hotkey recording are back.
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        for _ in 0..500 {
            while let Ok(done) = self.rx.try_recv() {
                self.finish(done);
            }
            if !self.devices_pending && !matches!(self.screen, Screen::Recording(_)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn fetch_devices(&mut self) {
        if self.devices_pending {
            return;
        }
        self.devices_pending = true;
        let control = Arc::clone(&self.control);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done::Devices(control.devices()));
        });
    }

    fn finish(&mut self, done: Done) {
        match done {
            Done::Devices(listed) => {
                self.devices_pending = false;
                let before = match &self.devices {
                    Some(Err(e)) => Some(e.clone()),
                    _ => None,
                };
                if let Err(e) = &listed
                    && before.as_ref() != Some(e)
                {
                    self.note(
                        Level::Error,
                        None,
                        Msg::new("tui.device.list_failed").arg("detail", e),
                    );
                }
                self.devices = Some(listed);
            }
            Done::Recorded(field, got) => {
                if self.screen == Screen::Recording(field) {
                    self.screen = Screen::Nav;
                }
                match got {
                    Ok(Some(combo)) => {
                        self.commit(field, &combo);
                    }
                    Ok(None) => {}
                    Err(e) => self.note(
                        Level::Error,
                        Some(field),
                        Msg::new("tui.log.record_failed").arg("detail", e),
                    ),
                }
            }
        }
    }

    /// There is something the user changed and has not saved.
    pub(crate) fn dirty(&self) -> bool {
        self.draft != self.base || !self.pending.is_empty()
    }

    pub(crate) fn note(&mut self, level: Level, field: Option<Field>, msg: Msg) {
        self.notes.push(Note {
            at: now_ms(),
            level,
            field,
            msg,
        });
        if self.notes.len() > NOTES_KEPT * 2 {
            self.notes.drain(..self.notes.len() - NOTES_KEPT);
        }
    }

    /// The field under the cursor.
    pub(crate) fn field(&self) -> Option<Field> {
        self.cursor.and_then(|i| FIELDS.get(i).copied())
    }

    /// The engine `id`'s entry.
    pub(crate) fn engine(&self, id: &str) -> Option<&EngineInfo> {
        self.engines.iter().find(|e| e.manifest.id == id)
    }

    /// Whether the draft's settings let engine `id` open a session.
    pub(crate) fn ready(&self, id: &str) -> bool {
        self.control
            .engine_ready(id, &self.draft.recognition.settings(id))
    }

    /// The text a field shows: typed text that does not parse, else its value.
    pub(crate) fn shown(&self, field: Field) -> String {
        self.pending
            .get(&field)
            .cloned()
            .unwrap_or_else(|| field.get(&self.draft))
    }

    // ---- polling -------------------------------------------------------

    /// Takes a config read from the running Anya: values the user has not
    /// touched follow it, edits in progress stay.
    pub(crate) fn take_config(&mut self, config: Config) {
        if config == self.base {
            return;
        }
        let mut draft = config.clone();
        for &f in FIELDS {
            if f != Field::EngineSettings && !same(f, &self.draft, &self.base) {
                f.copy(&self.draft, &mut draft);
            }
        }
        let ids: BTreeSet<String> = self
            .draft
            .recognition
            .engines
            .keys()
            .chain(self.base.recognition.engines.keys())
            .cloned()
            .collect();
        for id in ids {
            let mine = self.draft.recognition.engines.get(&id);
            if mine != self.base.recognition.engines.get(&id) {
                match mine {
                    Some(map) => {
                        draft.recognition.engines.insert(id, map.clone());
                    }
                    None => {
                        draft.recognition.engines.remove(&id);
                    }
                }
            }
        }
        self.set_base(config);
        self.draft = draft;
    }

    fn set_base(&mut self, config: Config) {
        if config.ui.language != self.base.ui.language {
            self.catalog = Catalog::for_setting(&config.ui.language);
        }
        self.base = config;
    }

    // ---- options -------------------------------------------------------

    /// A select's options, the configured value included even when unknown.
    pub(crate) fn options(&self, field: Field) -> Vec<Opt> {
        if matches!(field, Field::DictationDevice | Field::ContinuousDevice) {
            return self.device_options(field);
        }
        let (known, key): (Vec<&str>, LabelKey) = match field {
            Field::UiLanguage => (
                std::iter::once("system")
                    .chain(LOCALES.iter().copied())
                    .collect(),
                ui_language_key,
            ),
            Field::Mode => (vec!["toggle", "hold"], mode_key),
            Field::Output => (vec!["paste", "clipboard", "none"], output_key),
            Field::Language => (LANGUAGES.to_vec(), language_key),
            Field::Position => (POSITIONS.to_vec(), position_key),
            _ => return Vec::new(),
        };
        let mut values: Vec<String> = known.iter().map(|v| (*v).to_owned()).collect();
        for v in [field.get(&self.draft), field.get(&self.base)] {
            if !values.contains(&v) {
                values.push(v);
            }
        }
        values
            .into_iter()
            .map(|value| {
                let label = match key(&value) {
                    Some(k) => self.catalog.t(k).to_owned(),
                    None if value.is_empty() => self.catalog.t("tui.value.not_set").to_owned(),
                    None => value.clone(),
                };
                Opt {
                    value,
                    label,
                    group: Group::Plain,
                }
            })
            .collect()
    }

    /// The devices listed now, microphones then loopback devices, after the
    /// system default; a configured device that is not connected stays.
    fn device_options(&self, field: Field) -> Vec<Opt> {
        let mut out = vec![Opt {
            value: String::new(),
            label: self
                .catalog
                .t(match field {
                    Field::ContinuousDevice => "tui.device.default_system_sound",
                    _ => "tui.device.default_microphone",
                })
                .to_owned(),
            group: Group::Plain,
        }];
        let listed = self.listed();
        for (kind, group, key) in [
            (DeviceKind::Microphone, Group::Mic, "tui.device.mic"),
            (DeviceKind::Loopback, Group::Loopback, "tui.device.loopback"),
        ] {
            for d in listed.iter().filter(|d| d.kind == kind) {
                out.push(Opt {
                    value: d.id.clone(),
                    label: self.catalog.tf(key, &[("name", &d.name)]),
                    group,
                });
            }
        }
        for v in [field.get(&self.draft), field.get(&self.base)] {
            if is_default(&v)
                || listed.iter().any(|d| names(d, &v))
                || out.iter().any(|o| o.value == v)
            {
                continue;
            }
            // Until the first listing arrives, nobody knows it is missing.
            let label = match self.devices {
                Some(_) => self.catalog.tf("tui.device.missing", &[("name", &v)]),
                None => v.clone(),
            };
            out.push(Opt {
                value: v,
                label,
                group: Group::Missing,
            });
        }
        out
    }

    pub(crate) fn listed(&self) -> &[Device] {
        match &self.devices {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    /// Which of `options` the draft's value is.
    pub(crate) fn selected(&self, field: Field, options: &[Opt]) -> usize {
        let mut value = field.get(&self.draft);
        if matches!(field, Field::DictationDevice | Field::ContinuousDevice) {
            if is_default(&value) {
                return 0;
            }
            if let Some(d) = self.listed().iter().find(|d| names(d, &value)) {
                value.clone_from(&d.id);
            }
        }
        options.iter().position(|o| o.value == value).unwrap_or(0)
    }

    fn cycle(&mut self, field: Field, forward: bool) {
        let options = self.options(field);
        if options.is_empty() {
            return;
        }
        let at = self.selected(field, &options);
        let next = if forward {
            (at + 1) % options.len()
        } else {
            (at + options.len() - 1) % options.len()
        };
        field.assign(&mut self.draft, &options[next].value);
        self.errors.remove(&field);
    }

    // ---- keys ----------------------------------------------------------

    pub(crate) fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.done = true;
            return;
        }
        let armed = std::mem::take(&mut self.quit_armed);
        match self.screen.clone() {
            Screen::Nav => self.nav_key(key, armed),
            Screen::Text(edit) => self.text_key(key, edit),
            Screen::Select => self.select_key(key),
            // The OS has the keyboard; it ends on its own (Esc or timeout).
            Screen::Recording(_) => {}
            Screen::Picker(role, at) => self.picker_key(key, role, at),
            Screen::Settings(state) => self.settings_key(key, state),
        }
    }

    fn nav_key(&mut self, key: KeyEvent, armed: bool) {
        let last = FIELDS.len() - 1;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = Some(self.cursor.map_or(0, |c| (c + 1).min(last)));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = Some(self.cursor.map_or(0, |c| c.saturating_sub(1)));
            }
            KeyCode::Enter | KeyCode::Char('e' | 'i') => {
                let at = *self.cursor.get_or_insert(0);
                self.act(FIELDS[at]);
            }
            KeyCode::Char(' ') => {
                if let Some(f) = self.field()
                    && f.kind() == Kind::Switch
                {
                    self.flip(f);
                }
            }
            KeyCode::Char('r') => {
                if let Some(f @ (Field::Hotkeys | Field::ContinuousHotkey)) = self.field() {
                    self.record(f);
                }
            }
            KeyCode::Char('s') => self.save(),
            KeyCode::Char('h') => self.help = !self.help,
            KeyCode::Char('l') => self.log_expanded = !self.log_expanded,
            KeyCode::Char('o') => {
                let role = self
                    .field()
                    .and_then(Field::role)
                    .unwrap_or(EngineRole::Dictation);
                let id = self.draft.recognition.engine_id(role).to_owned();
                self.open_docs(&id, Some(Field::of_role(role)));
            }
            KeyCode::Char('q') => {
                if self.dirty() && !armed {
                    self.quit_armed = true;
                    self.note(Level::Warn, None, Msg::new("tui.log.unsaved_quit"));
                } else {
                    self.done = true;
                }
            }
            _ => {}
        }
    }

    fn act(&mut self, field: Field) {
        match field.kind() {
            Kind::Switch => self.flip(field),
            Kind::Select => self.screen = Screen::Select,
            Kind::Text => self.screen = Screen::Text(LineEdit::new(&self.shown(field))),
            Kind::Action => self.open_picker(field.role()),
        }
    }

    fn flip(&mut self, field: Field) {
        match field {
            Field::DictationEnabled => {
                let on = !self.base.dictation.enabled;
                let done = self.control.set_dictation(on);
                if self.applied(done) {
                    self.base.dictation.enabled = on;
                    self.draft.dictation.enabled = on;
                }
            }
            Field::ContinuousEnabled => {
                let on = !self.status.continuous_on;
                let done = self.control.set_continuous(on);
                if self.applied(done) {
                    self.base.continuous.enabled = on;
                    self.draft.continuous.enabled = on;
                }
                self.status = self.control.status();
            }
            _ => {
                let on = field.get(&self.draft) != "true";
                field.assign(&mut self.draft, &on.to_string());
            }
        }
    }

    /// Logs how a switch that saves at once went; true when it was written.
    fn applied(&mut self, done: Result<Saved, SaveError>) -> bool {
        match done {
            Ok(saved) => {
                self.saved_warning(&saved);
                true
            }
            Err(SaveError::Invalid(list)) => {
                let refused = list
                    .iter()
                    .map(|i| (self.field_of(i, &self.draft), Msg::invalid(i)))
                    .collect();
                self.refuse(refused);
                false
            }
            Err(SaveError::Write(detail)) => {
                self.note(
                    Level::Error,
                    None,
                    Msg::new("tui.log.save_failed").arg("detail", detail),
                );
                false
            }
        }
    }

    fn saved_warning(&mut self, saved: &Saved) {
        if let Some(detail) = &saved.hotkeys_failed {
            self.note(
                Level::Warn,
                None,
                Msg::new("tui.log.hotkeys_failed").arg("detail", detail),
            );
        }
    }

    fn select_key(&mut self, key: KeyEvent) {
        let Some(field) = self.field() else {
            self.screen = Screen::Nav;
            return;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.cycle(field, true),
            KeyCode::Char('k') | KeyCode::Up => self.cycle(field, false),
            KeyCode::Enter => {
                self.screen = Screen::Nav;
                self.save();
            }
            KeyCode::Esc => self.screen = Screen::Nav,
            _ => {}
        }
    }

    fn text_key(&mut self, key: KeyEvent, mut edit: LineEdit) {
        let Some(field) = self.field() else {
            self.screen = Screen::Nav;
            return;
        };
        match key.code {
            KeyCode::Enter => {
                if self.commit(field, &edit.text()) {
                    self.screen = Screen::Nav;
                    self.save();
                }
            }
            KeyCode::Esc => {
                // Kept, valid or not: a value that does not parse shows its
                // error and blocks saving until it is fixed.
                self.commit(field, &edit.text());
                self.screen = Screen::Nav;
            }
            _ => {
                edit.key(key);
                self.screen = Screen::Text(edit);
            }
        }
    }

    /// Puts typed text into the draft; false, with the reason shown, when it
    /// cannot be used.
    fn commit(&mut self, field: Field, text: &str) -> bool {
        let parsed = match field {
            Field::Hotkeys | Field::ContinuousHotkey => {
                let dictation = self.draft.dictation.hotkeys.clone();
                self.check(field, text, &dictation)
                    .map(|canonical| field.assign(&mut self.draft, &canonical))
                    .map_err(|invalid| Msg::invalid(&invalid))
            }
            _ => field.parse(&mut self.draft, text),
        };
        match parsed {
            Ok(()) => {
                self.pending.remove(&field);
                self.errors.remove(&field);
                true
            }
            Err(msg) => {
                self.pending.insert(field, text.to_owned());
                self.errors.insert(field, msg);
                false
            }
        }
    }

    /// A hotkey list in canonical form, or why `field` cannot use it. The
    /// continuous switch may not reuse a combo of `dictation`.
    fn check(&self, field: Field, value: &str, dictation: &str) -> Result<String, Invalid> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(String::new());
        }
        let role = match field {
            Field::ContinuousHotkey => HotkeyRole::Continuous,
            _ => HotkeyRole::Dictation,
        };
        let canonical = self.control.check_hotkeys(value, role)?;
        if field == Field::ContinuousHotkey {
            let taken = split_hotkeys(dictation);
            if let Some(combo) = split_hotkeys(&canonical)
                .into_iter()
                .find(|c| taken.iter().any(|t| t.eq_ignore_ascii_case(c)))
            {
                return Err(Invalid::HotkeyTaken { combo });
            }
        }
        Ok(canonical)
    }

    fn record(&mut self, field: Field) {
        self.screen = Screen::Recording(field);
        let control = Arc::clone(&self.control);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done::Recorded(field, control.record_hotkey(RECORD_FOR)));
        });
    }

    // ---- engines -------------------------------------------------------

    /// The engines in the order the picker lists them: those whose settings
    /// are complete first, the others after, each group in the engines' order.
    pub(crate) fn picker_order(&self) -> Vec<usize> {
        let (ready, rest): (Vec<usize>, Vec<usize>) =
            (0..self.engines.len()).partition(|&i| self.ready(&self.engines[i].manifest.id));
        [ready, rest].concat()
    }

    fn open_picker(&mut self, role: Option<EngineRole>) {
        if self.engines.is_empty() {
            self.note(Level::Error, None, Msg::new("tui.log.no_engines"));
            return;
        }
        let current = self
            .draft
            .recognition
            .engine_id(role.unwrap_or(EngineRole::Dictation));
        let at = self
            .picker_order()
            .iter()
            .position(|&i| self.engines[i].manifest.id == current)
            .unwrap_or(0);
        self.screen = Screen::Picker(role, at);
    }

    /// Opens engine `id`'s settings; `field` is where an unknown id is reported.
    fn open_settings(&mut self, id: &str, field: Option<Field>) {
        let Some(engine) = self.engine(id) else {
            self.note(
                Level::Error,
                field,
                Msg::new("tui.engine.unknown").arg("id", id),
            );
            return;
        };
        if engine.manifest.fields.is_empty() {
            let title = self.title(&engine.manifest);
            self.note(
                Level::Info,
                None,
                Msg::new("tui.log.no_settings").arg("engine", title),
            );
            return;
        }
        self.settings_scroll = 0;
        self.screen = Screen::Settings(SettingsState {
            engine: id.to_owned(),
            cursor: 0,
            edit: None,
        });
    }

    pub(crate) fn title(&self, manifest: &Manifest) -> String {
        manifest.title.get(self.catalog.locale()).to_owned()
    }

    fn open_docs(&mut self, id: &str, field: Option<Field>) {
        let Some(engine) = self.engine(id) else {
            self.note(
                Level::Error,
                field,
                Msg::new("tui.engine.unknown").arg("id", id),
            );
            return;
        };
        match engine.manifest.docs.clone() {
            Some(url) => {
                if let Err(e) = (self.opener)(&url) {
                    self.note(
                        Level::Error,
                        None,
                        Msg::new("tui.log.open_failed")
                            .arg("url", url)
                            .arg("detail", e),
                    );
                }
            }
            None => {
                let title = self.title(&engine.manifest);
                self.note(
                    Level::Info,
                    None,
                    Msg::new("tui.log.no_docs").arg("engine", title),
                );
            }
        }
    }

    fn picker_key(&mut self, key: KeyEvent, role: Option<EngineRole>, at: usize) {
        let order = self.picker_order();
        let n = order.len();
        if n == 0 {
            self.screen = Screen::Nav;
            return;
        }
        let field = role.map(Field::of_role);
        let id = self.engines[order[at % n]].manifest.id.clone();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.screen = Screen::Picker(role, (at + 1) % n);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.screen = Screen::Picker(role, (at + n - 1) % n);
            }
            KeyCode::Char('o') => self.open_docs(&id, field),
            KeyCode::Enter | KeyCode::Char('e' | 'i') => {
                self.screen = Screen::Nav;
                match (role, field) {
                    (Some(role), Some(field)) => {
                        self.draft.recognition.set_engine(role, &id);
                        self.errors.remove(&field);
                        if !self.ready(&id) {
                            self.open_settings(&id, Some(field));
                        }
                    }
                    _ => self.open_settings(&id, None),
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Nav,
            _ => {}
        }
    }

    fn settings_key(&mut self, key: KeyEvent, mut state: SettingsState) {
        let Some(fields) = self
            .engine(&state.engine)
            .map(|e| e.manifest.fields.clone())
        else {
            self.screen = Screen::Nav;
            return;
        };
        let n = fields.len();
        if n == 0 {
            self.screen = Screen::Nav;
            return;
        }
        let at = state.cursor % n;
        let field = &fields[at];
        let current = self
            .draft
            .recognition
            .settings(&state.engine)
            .get(&field.key)
            .cloned()
            .unwrap_or_default();
        match state.edit.take() {
            None => match key.code {
                KeyCode::Char('j') | KeyCode::Down => state.cursor = (at + 1) % n,
                KeyCode::Char('k') | KeyCode::Up => state.cursor = (at + n - 1) % n,
                KeyCode::Enter | KeyCode::Char('e' | 'i') => {
                    state.edit = Some(match &field.kind {
                        FieldKind::Choice { .. } => {
                            let list = choices(&field.kind, &current);
                            SettingEdit::Choice(
                                list.iter().position(|c| *c == current).unwrap_or(0),
                            )
                        }
                        FieldKind::Text | FieldKind::Secret => {
                            SettingEdit::Text(LineEdit::new(&current))
                        }
                    });
                }
                KeyCode::Char('s') => {
                    self.screen = Screen::Settings(state);
                    self.save();
                    return;
                }
                KeyCode::Char('o') => {
                    let id = state.engine.clone();
                    self.screen = Screen::Settings(state);
                    self.open_docs(&id, None);
                    return;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.screen = Screen::Nav;
                    return;
                }
                _ => {}
            },
            Some(SettingEdit::Text(mut edit)) => match key.code {
                KeyCode::Enter => self.set_setting(&state.engine, &field.key, &edit.text()),
                KeyCode::Esc => {}
                _ => {
                    edit.key(key);
                    state.edit = Some(SettingEdit::Text(edit));
                }
            },
            Some(SettingEdit::Choice(i)) => {
                let list = choices(&field.kind, &current);
                let len = list.len();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        state.edit = Some(SettingEdit::Choice((i + 1) % len));
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        state.edit = Some(SettingEdit::Choice((i + len - 1) % len));
                    }
                    KeyCode::Enter => {
                        let value = list.get(i).cloned().unwrap_or_default();
                        self.set_setting(&state.engine, &field.key, &value);
                    }
                    KeyCode::Esc => {}
                    _ => state.edit = Some(SettingEdit::Choice(i)),
                }
            }
        }
        self.screen = Screen::Settings(state);
    }

    /// Sets one of an engine's settings in the draft; blank removes the key,
    /// so the engine's default applies.
    fn set_setting(&mut self, engine: &str, key: &str, value: &str) {
        let value = value.trim();
        let engines = &mut self.draft.recognition.engines;
        let map = engines.entry(engine.to_owned()).or_default();
        if value.is_empty() {
            map.remove(key);
        } else {
            map.insert(key.to_owned(), value.to_owned());
        }
        if map.is_empty() {
            engines.remove(engine);
        }
    }

    // ---- saving --------------------------------------------------------

    /// Which field an [`Invalid`] from a save of `config` is about.
    fn field_of(&self, invalid: &Invalid, config: &Config) -> Field {
        let continuous = |value: &str| {
            split_hotkeys(&config.continuous.hotkey)
                .iter()
                .any(|c| c.eq_ignore_ascii_case(value.trim()))
                || config.continuous.hotkey.trim() == value.trim()
        };
        match invalid {
            Invalid::HotkeyUnparsable { value } if continuous(value) => Field::ContinuousHotkey,
            Invalid::HotkeyTextKey { combo } | Invalid::HotkeyFnFunctionKey { combo }
                if continuous(combo) =>
            {
                Field::ContinuousHotkey
            }
            Invalid::HotkeyTaken { .. } => Field::ContinuousHotkey,
            Invalid::HotkeyUnparsable { .. }
            | Invalid::HotkeyTextKey { .. }
            | Invalid::HotkeyFnFunctionKey { .. } => Field::Hotkeys,
            Invalid::UnknownPosition { .. } => Field::Position,
            Invalid::BadScale => Field::Scale,
            Invalid::UnknownEngine { role, .. } => Field::of_role(*role),
            Invalid::BadAddress { .. } => Field::Address,
        }
    }

    /// Shows why values were refused, beside their fields and in the log,
    /// and moves to the first of them.
    fn refuse(&mut self, refused: Vec<(Field, Msg)>) {
        let pending = &self.pending;
        self.errors.retain(|f, _| pending.contains_key(f));
        let first = refused.iter().map(|(f, _)| *f).min();
        for (field, msg) in refused {
            self.note(Level::Error, Some(field), msg.clone());
            self.errors.insert(field, msg);
        }
        if let Some(first) = first {
            self.cursor = FIELDS.iter().position(|f| *f == first);
            if !matches!(self.screen, Screen::Settings(_)) {
                self.screen = Screen::Nav;
            }
        }
    }

    /// Checks the draft, saves it through the running Anya and takes back
    /// what it applied.
    pub(crate) fn save(&mut self) {
        if !self.pending.is_empty() {
            let refused = self
                .pending
                .keys()
                .filter_map(|f| self.errors.get(f).map(|m| (*f, m.clone())))
                .collect();
            self.refuse(refused);
            return;
        }
        let mut config = self.draft.clone();
        let mut refused = Vec::new();
        match self.check(Field::Hotkeys, &config.dictation.hotkeys, "") {
            Ok(canonical) => config.dictation.hotkeys = canonical,
            Err(invalid) => refused.push((Field::Hotkeys, Msg::invalid(&invalid))),
        }
        let dictation = config.dictation.hotkeys.clone();
        match self.check(
            Field::ContinuousHotkey,
            &config.continuous.hotkey,
            &dictation,
        ) {
            Ok(canonical) => config.continuous.hotkey = canonical,
            Err(invalid) => refused.push((Field::ContinuousHotkey, Msg::invalid(&invalid))),
        }
        if !refused.is_empty() {
            self.refuse(refused);
            return;
        }
        match self.control.save(config.clone()) {
            Ok(saved) => {
                self.errors.clear();
                let applied = self.control.config();
                self.set_base(applied.clone());
                self.draft = applied;
                self.note(
                    Level::Ok,
                    None,
                    Msg::new("tui.log.saved")
                        .arg("path", self.info.config_path.display().to_string()),
                );
                self.saved_warning(&saved);
            }
            Err(SaveError::Invalid(list)) => {
                let refused = list
                    .iter()
                    .map(|i| (self.field_of(i, &config), Msg::invalid(i)))
                    .collect();
                self.refuse(refused);
            }
            Err(SaveError::Write(detail)) => self.note(
                Level::Error,
                None,
                Msg::new("tui.log.save_failed").arg("detail", detail),
            ),
        }
    }
}

/// `field` holds the same value in `a` and `b`.
fn same(field: Field, a: &Config, b: &Config) -> bool {
    let mut t = b.clone();
    field.copy(a, &mut t);
    t == *b
}
