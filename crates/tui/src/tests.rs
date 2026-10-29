//! What a user of the TUI sees and gets, against a fake running Anya.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_config::{Config, Mode};
use anya_core::audio::{Device, DeviceKind};
use anya_i18n::Catalog;
use anya_protocol::{Field as EngineField, FieldKind, Manifest, Source, Text};
use anya_runtime::{
    Control, Delivered, Dictation, EngineInfo, Entry, HotkeyRole, Info, Invalid, SaveError, Saved,
    Stats, Status, Subscription,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Screen, now_ms};
use crate::form::{FIELDS, Field};
use crate::view;

struct Fake {
    config: Mutex<Config>,
    saves: Mutex<Vec<Config>>,
    refuse: Mutex<Vec<Invalid>>,
    status: Mutex<Status>,
    activity: Mutex<Vec<Entry>>,
    devices: Mutex<Result<Vec<Device>, String>>,
    switched: Mutex<Vec<(&'static str, bool)>>,
    recorded: Mutex<Result<Option<String>, String>>,
}

fn engine(id: &str, title: &str, streaming: bool, fields: Vec<EngineField>) -> EngineInfo {
    EngineInfo {
        manifest: Manifest {
            id: id.to_owned(),
            title: Text::localized([("en", title), ("zh-CN", title)]),
            blurb: Text::localized([("en", "A test engine"), ("zh-CN", "测试引擎")]),
            docs: Some(format!("https://example.test/{id}")),
            streaming,
            fields,
        },
        ready: false,
    }
}

fn setting(key: &str, kind: FieldKind, required: bool) -> EngineField {
    EngineField {
        key: key.to_owned(),
        label: Text::from(key),
        help: Text::localized([("en", "Help text"), ("zh-CN", "帮助")]),
        kind,
        required,
        advanced: false,
    }
}

impl Fake {
    fn new() -> Arc<Fake> {
        let mut config = Config::default();
        config.dictation.hotkeys = "Alt+Super".into();
        config.recognition.engine = "stream".into();
        config.recognition.engines.insert(
            "stream".into(),
            [
                ("api_key".to_owned(), "sk-abcdefghijkl1234".to_owned()),
                ("model".to_owned(), "big".to_owned()),
                ("legacy".to_owned(), "kept".to_owned()),
            ]
            .into(),
        );
        Arc::new(Fake {
            config: Mutex::new(config),
            saves: Mutex::new(Vec::new()),
            refuse: Mutex::new(Vec::new()),
            status: Mutex::new(Status {
                dictation: Dictation::Idle,
                finishing: 0,
                live_text: None,
                continuous_on: false,
                subscription: Subscription::Listening {
                    url: "ws://127.0.0.1:7590/".into(),
                    subscribers: 2,
                },
                stats: Stats::default(),
            }),
            activity: Mutex::new(Vec::new()),
            devices: Mutex::new(Ok(vec![Device {
                id: "mic1".into(),
                name: "Built-in Mic".into(),
                kind: DeviceKind::Microphone,
            }])),
            switched: Mutex::new(Vec::new()),
            recorded: Mutex::new(Ok(None)),
        })
    }

    fn saves(&self) -> Vec<Config> {
        self.saves.lock().expect("lock").clone()
    }
}

impl Control for Fake {
    fn info(&self) -> Info {
        Info {
            platform: "macos",
            desktop: "darwin".into(),
            version: "0.0.0".into(),
            config_path: PathBuf::from("/home/u/.config/anya/config.toml"),
            debug: false,
        }
    }

    fn config(&self) -> Config {
        self.config.lock().expect("lock").clone()
    }

    fn save(&self, config: Config) -> Result<Saved, SaveError> {
        let refuse = self.refuse.lock().expect("lock").clone();
        if !refuse.is_empty() {
            return Err(SaveError::Invalid(refuse));
        }
        self.saves.lock().expect("lock").push(config.clone());
        *self.config.lock().expect("lock") = config;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn check_hotkeys(&self, value: &str, _role: HotkeyRole) -> Result<String, Invalid> {
        let mut out = Vec::new();
        for combo in crate::form::split_hotkeys(value) {
            match combo.as_str() {
                "Ctrl+A" => return Err(Invalid::HotkeyTextKey { combo }),
                "Super+Alt" => out.push("Alt+Super".to_owned()),
                _ => out.push(combo),
            }
        }
        Ok(out.join(", "))
    }

    fn engines(&self) -> Vec<EngineInfo> {
        vec![
            engine(
                "stream",
                "Stream",
                true,
                vec![
                    setting("api_key", FieldKind::Secret, true),
                    setting("model", FieldKind::Text, false),
                    setting(
                        "rate",
                        FieldKind::Choice {
                            options: vec!["16000".into(), "8000".into()],
                        },
                        false,
                    ),
                ],
            ),
            engine(
                "batch",
                "Batch",
                false,
                vec![setting("api_key", FieldKind::Secret, true)],
            ),
        ]
    }

    fn engine_ready(&self, id: &str, settings: &BTreeMap<String, String>) -> bool {
        self.engines()
            .iter()
            .find(|e| e.manifest.id == id)
            .is_some_and(|e| {
                e.manifest
                    .fields
                    .iter()
                    .filter(|f| f.required)
                    .all(|f| settings.get(&f.key).is_some_and(|v| !v.trim().is_empty()))
            })
    }

    fn devices(&self) -> Result<Vec<Device>, String> {
        self.devices.lock().expect("lock").clone()
    }

    fn status(&self) -> Status {
        self.status.lock().expect("lock").clone()
    }

    fn activity(&self) -> Vec<Entry> {
        self.activity.lock().expect("lock").clone()
    }

    fn set_dictation(&self, on: bool) -> Result<Saved, SaveError> {
        self.switched.lock().expect("lock").push(("dictation", on));
        self.config.lock().expect("lock").dictation.enabled = on;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn set_continuous(&self, on: bool) -> Result<Saved, SaveError> {
        self.switched.lock().expect("lock").push(("continuous", on));
        self.config.lock().expect("lock").continuous.enabled = on;
        self.status.lock().expect("lock").continuous_on = on;
        Ok(Saved {
            hotkeys_failed: None,
        })
    }

    fn suspend_hotkeys(&self, _period: Option<Duration>) {}

    fn record_hotkey(&self, _timeout: Duration) -> Result<Option<String>, String> {
        self.recorded.lock().expect("lock").clone()
    }

    fn copy(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn quit(&self) {}
}

fn app(fake: &Arc<Fake>, locale: &str) -> (App, Arc<Mutex<Vec<String>>>) {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&opened);
    let control: Arc<dyn Control> = Arc::clone(fake) as Arc<dyn Control>;
    let mut app = App::new(
        control,
        Catalog::new(locale),
        Box::new(move |url: &str| {
            log.lock().expect("lock").push(url.to_owned());
            Ok(())
        }),
    );
    app.settle();
    (app, opened)
}

fn press(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn ch(app: &mut App, c: char) {
    press(app, KeyCode::Char(c));
}

fn ctrl(app: &mut App, c: char) {
    app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        ch(app, c);
    }
}

/// Moves the cursor to `field` with j/k, as a user would.
fn goto(app: &mut App, field: Field) {
    for _ in 0..FIELDS.len() {
        ch(app, 'k');
    }
    let at = FIELDS.iter().position(|f| *f == field).expect("listed");
    for _ in 0..at {
        ch(app, 'j');
    }
    assert_eq!(app.field(), Some(field));
}

/// The rendered screen as text, one line per row.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal.draw(|frame| view::draw(app, frame)).expect("draw");
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..height {
        let mut skip = 0;
        for x in 0..width {
            let symbol = buffer.cell((x, y)).map_or(" ", |c| c.symbol());
            if skip > 0 {
                skip -= 1;
                continue;
            }
            out.push_str(symbol);
            skip = symbol.width().saturating_sub(1);
        }
        out.push('\n');
    }
    out
}

fn shows(app: &mut App, text: &str) {
    let s = screen(app, 160, 60);
    assert!(s.contains(text), "{text:?} not on screen:\n{s}");
}

#[test]
fn the_main_screen_reads_in_both_locales() {
    let fake = Fake::new();
    {
        let mut status = fake.status.lock().expect("lock");
        status.dictation = Dictation::Recording;
        status.live_text = Some("hello wor".into());
        status.stats = Stats {
            sessions: 3,
            chars: 120,
            audio_ms: 60_000,
            last_chars: 30,
            last_audio_ms: 30_000,
        };
    }
    fake.activity.lock().expect("lock").push(Entry::Final {
        at: now_ms(),
        source: Source::Dictation,
        text: "hello world".into(),
        delivered: Delivered::Pasted,
    });
    let cases = [
        (
            "en",
            [
                "Sessions 3  |  Characters 120  |  Average 120 chars/min  |  Latest 60 chars/min",
                "── Settings ──",
                "Hotkeys: Alt+Super",
                "Engine: Stream [Streaming] (Enter to choose)",
                "Engine settings: Set up (Enter to change)",
                "Listening on ws://127.0.0.1:7590/ · subscribers: 2",
                "Recording",
                "Recognizing: hello wor",
                "Dictation · pasted: hello world",
            ],
        ),
        (
            "zh-CN",
            [
                "历史 3 次  |  总字数 120  |  平均速度 120 字/分钟  |  最近速度 60 字/分钟",
                "── 配置 ──",
                "热键: Alt+Super",
                "引擎: Stream [流式] (回车选择)",
                "引擎配置: 已配置 (回车修改)",
                "监听中 ws://127.0.0.1:7590/ · 2 个订阅者",
                "录音中",
                "识别中：hello wor",
                "听写 · 已上屏：hello world",
            ],
        ),
    ];
    for (locale, wanted) in cases {
        let (mut app, _) = app(&fake, locale);
        for text in wanted {
            shows(&mut app, text);
        }
    }
}

#[test]
fn a_typed_field_is_checked_then_saved() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::StopDelay);
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    typed(&mut app, "-5");
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::Text(_)), "still editing");
    shows(
        &mut app,
        "The stop delay must be a whole number of milliseconds, 0 or more",
    );
    assert!(fake.saves().is_empty(), "a refused value saves nothing");

    // Esc keeps the bad text; s refuses to save it.
    press(&mut app, KeyCode::Esc);
    ch(&mut app, 's');
    assert!(fake.saves().is_empty());
    assert!(app.dirty());

    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    typed(&mut app, "250");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Nav);
    let saves = fake.saves();
    assert_eq!(saves.len(), 1);
    assert_eq!(saves[0].dictation.stop_delay_ms, 250);
    assert!(!app.dirty());
    shows(&mut app, "Saved to /home/u/.config/anya/config.toml");
    shows(&mut app, "Stop delay (ms): 250");
}

#[test]
fn a_select_saves_the_value_it_shows() {
    let fake = Fake::new();
    fake.config.lock().expect("lock").overlay.position = "middle".into();
    let (mut app, _) = app(&fake, "en");
    // Enter twice on a select with a value outside its list keeps that value.
    goto(&mut app, Field::Position);
    press(&mut app, KeyCode::Enter);
    shows(&mut app, "Position: [middle ▲▼]");
    press(&mut app, KeyCode::Enter);
    assert_eq!(fake.saves()[0].overlay.position, "middle");

    goto(&mut app, Field::Mode);
    press(&mut app, KeyCode::Enter);
    ch(&mut app, 'j');
    shows(&mut app, "Mode: [Hold ▲▼]");
    press(&mut app, KeyCode::Esc);
    shows(&mut app, "● Unsaved changes");
    ch(&mut app, 's');
    assert_eq!(fake.saves()[1].dictation.mode, Mode::Hold);
}

#[test]
fn a_refused_hotkey_shows_why_in_each_locale() {
    for locale in ["en", "zh-CN"] {
        let fake = Fake::new();
        let (mut app, _) = app(&fake, locale);
        goto(&mut app, Field::Hotkeys);
        press(&mut app, KeyCode::Enter);
        ctrl(&mut app, 'u');
        typed(&mut app, "Ctrl+A");
        press(&mut app, KeyCode::Enter);
        let catalog = Catalog::new(locale);
        let message = catalog.tf("error.invalid.hotkey_text_key", &[("combo", "Ctrl+A")]);
        assert!(!message.contains('{'));
        shows(&mut app, &message);
        press(&mut app, KeyCode::Esc);
        ch(&mut app, 's');
        assert!(fake.saves().is_empty(), "{locale}: nothing saved");
        assert_eq!(app.draft.dictation.hotkeys, "Alt+Super");
    }
}

#[test]
fn the_switch_hotkey_may_not_reuse_a_dictation_hotkey() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::ContinuousHotkey);
    press(&mut app, KeyCode::Enter);
    typed(&mut app, "Super+Alt");
    press(&mut app, KeyCode::Enter);
    shows(
        &mut app,
        "Alt+Super is already a dictation hotkey and cannot also switch continuous transcription",
    );
    assert!(fake.saves().is_empty());
}

#[test]
fn a_value_anya_refuses_shows_beside_its_field() {
    let fake = Fake::new();
    *fake.refuse.lock().expect("lock") = vec![Invalid::BadAddress {
        address: "0.0.0.0:1".into(),
        detail: "not loopback".into(),
    }];
    let (mut app, _) = app(&fake, "zh-CN");
    goto(&mut app, Field::Address);
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    typed(&mut app, "0.0.0.0:1");
    assert!(matches!(&app.screen, Screen::Text(e) if e.text() == "0.0.0.0:1"));
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.field(), Some(Field::Address));
    let message = Catalog::new("zh-CN").tf(
        "error.invalid.address",
        &[("address", "0.0.0.0:1"), ("detail", "not loopback")],
    );
    shows(&mut app, &format!("    {message}"));
    shows(&mut app, &format!("订阅地址：{message}"));
    assert!(app.dirty(), "the edit stays to be fixed");
}

#[test]
fn the_engine_modal_writes_only_declared_keys_and_deletes_blanked_ones() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::EngineSettings);
    press(&mut app, KeyCode::Enter);
    let s = screen(&mut app, 160, 60);
    assert!(s.contains("Engine settings · Stream"), "{s}");
    assert!(s.contains("api_key*: ••••••••1234"), "{s}");
    assert!(s.contains("rate: (default)"), "{s}");
    assert!(
        !s.contains("sk-abcdefghijkl1234"),
        "a secret is never shown"
    );

    // api_key: replaced.
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    typed(&mut app, "sk-new");
    assert!(
        !screen(&mut app, 160, 60).contains("sk-new"),
        "masked while typed"
    );
    press(&mut app, KeyCode::Enter);
    // model: blanked, so removed.
    ch(&mut app, 'j');
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    press(&mut app, KeyCode::Enter);
    // rate: unset, Enter twice keeps it unset.
    ch(&mut app, 'j');
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    ch(&mut app, 's');

    let saved = &fake.saves()[0].recognition.engines["stream"];
    let want: BTreeMap<String, String> = [
        ("api_key".to_owned(), "sk-new".to_owned()),
        ("legacy".to_owned(), "kept".to_owned()),
    ]
    .into();
    assert_eq!(*saved, want);
    assert!(
        matches!(app.screen, Screen::Settings(_)),
        "s keeps the modal"
    );
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.screen, Screen::Nav);
}

#[test]
fn the_picker_chooses_an_engine_and_opens_its_docs() {
    let fake = Fake::new();
    let (mut app, opened) = app(&fake, "en");
    goto(&mut app, Field::Engine);
    press(&mut app, KeyCode::Enter);
    let s = screen(&mut app, 160, 60);
    assert!(s.contains("Choose a recognition engine"), "{s}");
    assert!(s.contains("▸ ● Stream  [Streaming]  Set up"), "{s}");
    assert!(s.contains("Batch  [Whole clip]  Not set up"), "{s}");
    ch(&mut app, 'o');
    assert_eq!(
        *opened.lock().expect("lock"),
        ["https://example.test/stream"]
    );
    ch(&mut app, 'j');
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.draft.recognition.engine, "batch");
    assert!(matches!(&app.screen, Screen::Settings(s) if s.engine == "batch"));
    press(&mut app, KeyCode::Esc);
    shows(
        &mut app,
        "Engine settings: Required settings missing (Enter to set up)",
    );
    assert!(fake.saves().is_empty(), "picking is not saving");
    ch(&mut app, 's');
    assert_eq!(fake.saves()[0].recognition.engine, "batch");
}

#[test]
fn quitting_asks_again_only_with_unsaved_changes() {
    let fake = Fake::new();
    let (mut clean, _) = app(&fake, "en");
    ch(&mut clean, 'q');
    assert!(clean.done());

    let (mut edited, _) = app(&fake, "en");
    goto(&mut edited, Field::SilenceGate);
    ch(&mut edited, ' ');
    assert!(edited.dirty());
    ch(&mut edited, 'q');
    assert!(!edited.done());
    shows(&mut edited, "There are unsaved changes: press q again");
    ch(&mut edited, 'q');
    assert!(edited.done());

    let (mut interrupted, _) = app(&fake, "en");
    goto(&mut interrupted, Field::SilenceGate);
    ch(&mut interrupted, ' ');
    ctrl(&mut interrupted, 'c');
    assert!(interrupted.done(), "Ctrl+C quits at once");
}

#[test]
fn both_device_lists_follow_what_is_connected() {
    let fake = Fake::new();
    fake.config.lock().expect("lock").dictation.device = "USB Mic".into();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::DictationDevice);
    press(&mut app, KeyCode::Enter);
    shows(&mut app, "Input device: [USB Mic (not detected) ▲▼]");
    shows(&mut app, "Built-in Mic · Microphone");
    press(&mut app, KeyCode::Esc);

    fake.devices
        .lock()
        .expect("lock")
        .as_mut()
        .expect("ok")
        .push(Device {
            id: "bh".into(),
            name: "BlackHole 2ch".into(),
            kind: DeviceKind::Loopback,
        });
    app.refresh();
    app.settle();
    goto(&mut app, Field::ContinuousDevice);
    press(&mut app, KeyCode::Enter);
    shows(
        &mut app,
        "Loopback devices (record what this computer plays)",
    );
    ch(&mut app, 'j');
    ch(&mut app, 'j');
    shows(&mut app, "Source device: [BlackHole 2ch · Loopback ▲▼]");
    press(&mut app, KeyCode::Enter);
    let saved = &fake.saves()[0];
    assert_eq!(saved.continuous.device, "bh");
    assert_eq!(saved.dictation.device, "USB Mic", "an absent device stays");
}

#[test]
fn a_device_list_error_is_logged_once() {
    let fake = Fake::new();
    *fake.devices.lock().expect("lock") = Err("no audio server".into());
    let (mut app, _) = app(&fake, "en");
    app.refresh();
    app.settle();
    let s = screen(&mut app, 160, 60);
    assert_eq!(
        s.matches("Cannot list input devices: no audio server")
            .count(),
        1,
        "{s}"
    );
}

#[test]
fn changes_made_elsewhere_show_without_losing_edits() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::Gain);
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    typed(&mut app, "3");
    press(&mut app, KeyCode::Esc);
    {
        let mut config = fake.config.lock().expect("lock");
        config.overlay.position = "top-left".into();
        config.audio.gain = -2;
    }
    app.refresh();
    assert_eq!(app.draft.overlay.position, "top-left");
    assert_eq!(app.draft.audio.gain, 3, "the edit in progress stays");
    shows(&mut app, "Position: Top left");
}

#[test]
fn the_dictation_switch_applies_at_once() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::DictationEnabled);
    ch(&mut app, ' ');
    assert_eq!(*fake.switched.lock().expect("lock"), [("dictation", false)]);
    assert!(!app.dirty(), "nothing left to save");
    shows(&mut app, "Voice input: ○ Off  (Space)");
    shows(&mut app, "Dictation hotkeys are off");

    goto(&mut app, Field::ContinuousEnabled);
    press(&mut app, KeyCode::Enter);
    shows(&mut app, "Continuous transcription: running");
    assert!(fake.saves().is_empty());
}

#[test]
fn a_recorded_hotkey_fills_its_field() {
    let fake = Fake::new();
    *fake.recorded.lock().expect("lock") = Ok(Some("Super+Alt".into()));
    fake.config.lock().expect("lock").dictation.hotkeys = "F9".into();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::Hotkeys);
    ch(&mut app, 'r');
    assert_eq!(app.screen, Screen::Recording(Field::Hotkeys));
    app.settle();
    assert_eq!(app.screen, Screen::Nav);
    assert_eq!(app.draft.dictation.hotkeys, "Alt+Super");
    ch(&mut app, 's');
    assert_eq!(fake.saves()[0].dictation.hotkeys, "Alt+Super");
}

#[test]
fn saving_another_interface_language_switches_the_screen() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::UiLanguage);
    press(&mut app, KeyCode::Enter);
    ch(&mut app, 'j');
    ch(&mut app, 'j');
    shows(&mut app, "Interface language: [简体中文 ▲▼]");
    press(&mut app, KeyCode::Enter);
    assert_eq!(fake.saves()[0].ui.language, "zh-CN");
    assert_eq!(app.catalog.locale(), "zh-CN");
    shows(&mut app, "界面语言: 简体中文");
}
