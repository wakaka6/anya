//! What a user of the TUI sees and gets, against a fake running Anya.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_config::{Config, EngineRole, Mode};
use anya_core::audio::{Device, DeviceKind};
use anya_core::hooks::HookFailure;
use anya_i18n::Catalog;
use anya_protocol::{Field as EngineField, FieldKind, Manifest, Provide, Scope, Source, Text};
use anya_runtime::{
    BuiltinHooks, ClientIssue, Control, Delivered, Dictation, EngineInfo, Entry, HookInfo,
    HotkeyRole, Info, InstalledPlugin, Invalid, Lan, MarketPlugin, PairError, PairedClient,
    Pairing, PluginIssue, PluginLoad, PreviewIssue, PreviewView, SaveError, Saved, Stats, Status,
    Subscription, UpdateIssue, UpdateState, UpdateVia,
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
    plugins: Mutex<Vec<InstalledPlugin>>,
    activity: Mutex<Vec<Entry>>,
    devices: Mutex<Result<Vec<Device>, String>>,
    switched: Mutex<Vec<(&'static str, bool)>>,
    recorded: Mutex<Result<Option<String>, String>>,
    update: Mutex<UpdateState>,
    upgrade_result: Mutex<Result<(), UpdateIssue>>,
    update_checks: Mutex<u32>,
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
        enabled: None,
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
        for role in EngineRole::ALL {
            config.recognition.set_engine(role, "stream");
        }
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
                level: None,
                continuous_on: false,
                subscription: Subscription::Listening {
                    url: "ws://127.0.0.1:7590/".into(),
                    subscribers: 2,
                },
                lan: Lan::Off,
                stats: Stats::default(),
            }),
            plugins: Mutex::new(Vec::new()),
            activity: Mutex::new(Vec::new()),
            devices: Mutex::new(Ok(vec![Device {
                id: "mic1".into(),
                name: "Built-in Mic".into(),
                kind: DeviceKind::Microphone,
            }])),
            switched: Mutex::new(Vec::new()),
            recorded: Mutex::new(Ok(None)),
            update: Mutex::new(UpdateState::Unknown),
            upgrade_result: Mutex::new(Ok(())),
            update_checks: Mutex::new(0),
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
            hotwords_path: PathBuf::from("/home/u/.config/anya/hotwords.txt"),
            replacements_path: PathBuf::from("/home/u/.config/anya/replacements.txt"),
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

    fn clients(&self) -> Vec<PairedClient> {
        Vec::new()
    }

    fn start_pairing(&self) -> Result<Pairing, PairError> {
        Err(PairError::LanOff)
    }

    fn set_client_scopes(&self, _id: &str, _scopes: Vec<Scope>) -> Result<(), ClientIssue> {
        Err(ClientIssue::Write {
            detail: "no paired clients".to_owned(),
        })
    }

    fn remove_client(&self, _id: &str) -> Result<(), ClientIssue> {
        Err(ClientIssue::Write {
            detail: "no paired clients".to_owned(),
        })
    }

    fn plugins(&self) -> Vec<InstalledPlugin> {
        self.plugins.lock().expect("lock").clone()
    }

    fn plugin_market(&self) -> Result<Vec<MarketPlugin>, PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no registry".to_owned(),
        })
    }

    fn install_plugin(&self, _id: &str) -> Result<(), PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no registry".to_owned(),
        })
    }

    fn set_plugin_enabled(&self, _id: &str, _on: bool) -> Result<(), PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no plugins".to_owned(),
        })
    }

    fn hooks(&self) -> Vec<HookInfo> {
        Vec::new()
    }

    fn builtin_hooks(&self) -> Vec<BuiltinHooks> {
        Vec::new()
    }

    fn remove_plugin(&self, _id: &str) -> Result<(), PluginIssue> {
        Err(PluginIssue::Bun {
            reason: "no plugins".to_owned(),
        })
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

    fn preview_overlay(&self, _view: PreviewView) -> Result<(), PreviewIssue> {
        Ok(())
    }

    fn retry_dictation(&self) {}

    fn dismiss_dictation(&self) {}

    fn copy(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn quit(&self) {}

    fn update(&self) -> UpdateState {
        self.update.lock().expect("lock").clone()
    }

    fn check_update(&self) {
        *self.update_checks.lock().expect("lock") += 1;
    }

    fn upgrade(&self) -> Result<(), UpdateIssue> {
        let result = self.upgrade_result.lock().expect("lock").clone();
        if result.is_ok() {
            *self.update.lock().expect("lock") = UpdateState::Restart {
                version: "0.9.0".into(),
            };
        }
        result
    }
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

/// `label` and then `value` on one row of the screen, whatever lies between.
fn shows_row(app: &mut App, label: &str, value: &str) {
    let s = screen(app, 160, 60);
    let found = s.lines().any(|row| {
        row.find(label)
            .is_some_and(|at| row[at + label.len()..].contains(value))
    });
    assert!(found, "{label:?} then {value:?} not on one row:\n{s}");
}

/// What the two columns of panels say, their wrapped rows joined: once with
/// a space between rows (English wraps at spaces) and once with none
/// (Chinese wraps anywhere).
fn column_text(screen: &str) -> Vec<String> {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for row in screen.lines() {
        let (l, r) = row.split_once("││").unwrap_or((row, ""));
        let trim = |t: &str| t.trim_matches(['│', ' ']).to_owned();
        left.push(trim(l));
        right.push(trim(r));
    }
    vec![
        left.join(" "),
        left.join(""),
        right.join(" "),
        right.join(""),
    ]
}

fn shows(app: &mut App, text: &str) {
    let s = screen(app, 160, 60);
    let wrapped = column_text(&s);
    let squeeze = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let text_squeezed = squeeze(text);
    assert!(
        s.contains(text) || wrapped.iter().any(|w| squeeze(w).contains(&text_squeezed)),
        "{text:?} not on screen:\n{s}"
    );
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
        session: None,
        engine: None,
    });
    let cases = [
        (
            "en",
            [
                "Sessions 3  |  Characters 120  |  Average 120 chars/min  |  Latest 60 chars/min",
                "Recognition",
                "Recording",
                "Recognizing: hello wor",
                "✓ hello world",
            ],
            "Listening on ws://127.0.0.1:7590/ · subscribers: 2",
            ("Hotkeys", "Alt+Super"),
            [
                ("Dictation engine", "Stream [Streaming] Set up"),
                ("Remote engine", "Stream [Streaming] Set up"),
                ("Engine settings", "(Enter to pick an engine to set up)"),
            ],
        ),
        (
            "zh-CN",
            [
                "历史 3 次  |  总字数 120  |  平均速度 120 字/分钟  |  最近速度 60 字/分钟",
                "识别",
                "录音中",
                "识别中：hello wor",
                "✓ hello world",
            ],
            "监听中 ws://127.0.0.1:7590/ · 2 个订阅者",
            ("热键", "Alt+Super"),
            [
                ("听写引擎", "Stream [流式] 已配置"),
                ("远程引擎", "Stream [流式] 已配置"),
                ("引擎配置", "(回车选引擎并配置)"),
            ],
        ),
    ];
    for (locale, wanted, listening, hotkeys, engines) in cases {
        let (mut app, _) = app(&fake, locale);
        for text in wanted {
            shows(&mut app, text);
        }
        shows_row(&mut app, hotkeys.0, hotkeys.1);
        press(&mut app, KeyCode::Right);
        for (label, value) in engines {
            shows_row(&mut app, label, value);
        }
        press(&mut app, KeyCode::Right);
        shows(&mut app, listening);
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
    shows_row(&mut app, "Stop delay (ms)", "250");
}

#[test]
fn installed_plugins_show_their_load_state_in_both_locales() {
    for (locale, ready, failed) in [
        (
            "en",
            "echo (engine+transform; ready)",
            "broken (failed: no entry)",
        ),
        (
            "zh-CN",
            "echo（引擎+文本转换；已就绪）",
            "broken（失败：no entry）",
        ),
    ] {
        let fake = Fake::new();
        *fake.plugins.lock().expect("lock") = ["echo", "broken"]
            .into_iter()
            .map(|id| InstalledPlugin {
                id: id.to_owned(),
                version: "1.0.0".to_owned(),
                enabled: true,
                load: if id == "echo" {
                    PluginLoad::Ready
                } else {
                    PluginLoad::Failed {
                        detail: "no entry".to_owned(),
                    }
                },
                title: None,
                description: None,
                provides: if id == "echo" {
                    vec![Provide::Engine, Provide::Transform]
                } else {
                    Vec::new()
                },
                host: true,
                permissions: Vec::new(),
                fields: Vec::new(),
                missing: Vec::new(),
            })
            .collect();
        let (mut app, _) = app(&fake, locale);
        shows(&mut app, ready);
        shows(&mut app, failed);
    }
}

#[test]
fn failed_hooks_and_stalled_plugins_show_in_the_log_in_both_locales() {
    for (locale, failed, stalled) in [
        (
            "en",
            "Plugin polish: its transform hook took too long",
            "Plugin polish stopped answering and was restarted",
        ),
        (
            "zh-CN",
            "插件 polish：文本转换钩子超时",
            "插件 polish 停止响应，已重启",
        ),
    ] {
        let fake = Fake::new();
        {
            let mut log = fake.activity.lock().expect("lock");
            log.push(Entry::HookFailed {
                at: now_ms(),
                plugin: "polish".to_owned(),
                hook: Provide::Transform,
                failure: HookFailure::Timeout,
                session: None,
            });
            log.push(Entry::PluginStalled {
                at: now_ms(),
                plugin: "polish".to_owned(),
            });
        }
        let (mut app, _) = app(&fake, locale);
        shows(&mut app, failed);
        shows(&mut app, stalled);
    }
}

#[test]
fn lan_sharing_is_a_switch_and_a_port_with_its_status() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::LanEnabled);
    shows(&mut app, "LAN sharing");
    press(&mut app, KeyCode::Char(' '));
    goto(&mut app, Field::LanPort);
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    typed(&mut app, "99999");
    press(&mut app, KeyCode::Enter);
    shows(&mut app, "The port must be a number from 1 to 65535");
    assert!(fake.saves().is_empty(), "a refused port saves nothing");
    ctrl(&mut app, 'u');
    typed(&mut app, "9000");
    press(&mut app, KeyCode::Enter);
    let saves = fake.saves();
    assert!(saves[0].subscription.lan);
    assert_eq!(saves[0].subscription.lan_port, 9000);

    fake.status.lock().expect("lock").lan = Lan::Listening {
        url: "wss://192.168.1.5:9000/asr".into(),
        fingerprint: "ab:cd".into(),
    };
    app.refresh();
    shows(
        &mut app,
        "LAN: wss://192.168.1.5:9000/asr · certificate ab:cd",
    );
    fake.status.lock().expect("lock").lan = Lan::Down {
        detail: "address in use".into(),
    };
    app.refresh();
    shows(&mut app, "LAN sharing is not listening: address in use");
}

#[test]
fn a_select_saves_the_value_it_shows() {
    let fake = Fake::new();
    fake.config.lock().expect("lock").overlay.position = "middle".into();
    let (mut app, _) = app(&fake, "en");
    // Enter twice on a select with a value outside its list keeps that value.
    goto(&mut app, Field::Position);
    press(&mut app, KeyCode::Enter);
    shows_row(&mut app, "Position", "[middle ▲▼]");
    press(&mut app, KeyCode::Enter);
    assert_eq!(fake.saves()[0].overlay.position, "middle");

    goto(&mut app, Field::Mode);
    press(&mut app, KeyCode::Enter);
    ch(&mut app, 'j');
    shows_row(&mut app, "Mode", "[Hold ▲▼]");
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
fn an_unknown_engine_is_shown_beside_the_use_that_chose_it() {
    let fake = Fake::new();
    *fake.refuse.lock().expect("lock") = vec![Invalid::UnknownEngine {
        role: EngineRole::Remote,
        id: "nope".into(),
    }];
    let (mut app, _) = app(&fake, "en");
    ch(&mut app, 's');
    assert_eq!(app.field(), Some(Field::RemoteEngine));
    shows_row(&mut app, "Remote engine", "Stream");
    shows(&mut app, "Unknown recognition engine: nope");
}

#[test]
fn the_engine_modal_writes_only_declared_keys_and_deletes_blanked_ones() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::EngineSettings);
    press(&mut app, KeyCode::Enter);
    let s = screen(&mut app, 160, 60);
    assert!(s.contains("Choose an engine to set up"), "{s}");
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
    goto(&mut app, Field::DictationEngine);
    press(&mut app, KeyCode::Enter);
    let s = screen(&mut app, 160, 60);
    assert!(s.contains("Engine for dictation"), "{s}");
    assert!(s.contains("▸ ● Stream  [Streaming]  Set up"), "{s}");
    assert!(s.contains("Batch  [Whole clip]  Not set up"), "{s}");
    ch(&mut app, 'o');
    assert_eq!(
        *opened.lock().expect("lock"),
        ["https://example.test/stream"]
    );
    ch(&mut app, 'j');
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.draft.recognition.dictation, "batch");
    assert_eq!(
        app.draft.recognition.continuous, "stream",
        "other uses stay"
    );
    assert_eq!(app.draft.recognition.remote, "stream");
    assert!(matches!(&app.screen, Screen::Settings(s) if s.engine == "batch"));
    press(&mut app, KeyCode::Esc);
    assert!(fake.saves().is_empty(), "picking is not saving");
    ch(&mut app, 's');
    let saved = &fake.saves()[0].recognition;
    assert_eq!(
        (
            saved.dictation.as_str(),
            saved.continuous.as_str(),
            saved.remote.as_str()
        ),
        ("batch", "stream", "stream")
    );
}

#[test]
fn each_use_picks_its_own_engine_with_the_set_up_ones_first() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    // Batch has no key yet, so Stream is offered first for every use.
    goto(&mut app, Field::RemoteEngine);
    shows_row(&mut app, "Remote engine", "Stream");
    press(&mut app, KeyCode::Enter);
    let s = screen(&mut app, 160, 60);
    assert!(s.contains("Engine for remote dictation"), "{s}");
    ch(&mut app, 'j');
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.draft.recognition.remote, "batch");
    assert_eq!(app.draft.recognition.dictation, "stream");
    assert_eq!(app.draft.recognition.continuous, "stream");
    press(&mut app, KeyCode::Esc);

    goto(&mut app, Field::ContinuousEngine);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.draft.recognition.continuous, "stream");
    assert_eq!(app.screen, Screen::Nav, "a set-up engine needs no settings");
    ch(&mut app, 's');
    let saved = &fake.saves()[0].recognition;
    assert_eq!(
        (
            saved.dictation.as_str(),
            saved.continuous.as_str(),
            saved.remote.as_str()
        ),
        ("stream", "stream", "batch")
    );
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
    shows_row(&mut app, "Input device", "[USB Mic (not detected) ▲▼]");
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
    shows_row(&mut app, "Source device", "[BlackHole 2ch · Loopback ▲▼]");
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
    let joined = column_text(&s).swap_remove(2);
    assert_eq!(
        joined
            .matches("Cannot list input devices: no audio server")
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
    goto(&mut app, Field::Position);
    shows_row(&mut app, "Position", "Top left");
}

#[test]
fn the_dictation_switch_applies_at_once() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::DictationEnabled);
    ch(&mut app, ' ');
    assert_eq!(*fake.switched.lock().expect("lock"), [("dictation", false)]);
    assert!(!app.dirty(), "nothing left to save");
    shows_row(&mut app, "Voice input", "○ Off");
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
    shows_row(&mut app, "Interface language", "[简体中文 ▲▼]");
    press(&mut app, KeyCode::Enter);
    assert_eq!(fake.saves()[0].ui.language, "zh-CN");
    assert_eq!(app.catalog.locale(), "zh-CN");
    shows_row(&mut app, "界面语言", "简体中文");
}

fn available(via: UpdateVia) -> UpdateState {
    UpdateState::Available {
        version: "0.9.0".into(),
        via,
    }
}

#[test]
fn each_update_state_shows_its_line_and_keys_in_both_locales() {
    for (locale, brew, other, upgrading, restart) in [
        (
            "en",
            "Anya 0.9.0 is available. Press u to upgrade.",
            "Anya 0.9.0 is available. Update it where you installed Anya from.",
            "Upgrading to Anya 0.9.0",
            "Upgraded to Anya 0.9.0; restart Anya to use it.",
        ),
        (
            "zh-CN",
            "Anya 0.9.0 已发布。按 u 升级。",
            "Anya 0.9.0 已发布。请到安装 Anya 的来源处更新。",
            "正在升级到 Anya 0.9.0",
            "已升级到 Anya 0.9.0；重启 Anya 后生效。",
        ),
    ] {
        let fake = Fake::new();
        let (mut app, _) = app(&fake, locale);
        for state in [
            UpdateState::Unknown,
            UpdateState::Current,
            UpdateState::Failed {
                detail: "timed out".into(),
            },
        ] {
            *fake.update.lock().expect("lock") = state;
            app.refresh();
            assert!(crate::view::update_notice(&app).is_none());
        }
        *fake.update.lock().expect("lock") = available(UpdateVia::Other);
        app.refresh();
        shows(&mut app, other);
        ch(&mut app, 'u');
        app.settle();
        assert_eq!(
            *fake.update.lock().expect("lock"),
            available(UpdateVia::Other)
        );
        assert!(!screen(&mut app, 160, 60).contains(brew));

        *fake.update.lock().expect("lock") = available(UpdateVia::Homebrew);
        app.refresh();
        shows(&mut app, brew);
        *fake.update.lock().expect("lock") = UpdateState::Upgrading {
            version: "0.9.0".into(),
        };
        app.refresh();
        shows(&mut app, upgrading);
        *fake.update.lock().expect("lock") = UpdateState::Restart {
            version: "0.9.0".into(),
        };
        app.refresh();
        shows(&mut app, restart);
    }
}

#[test]
fn a_failed_check_shows_in_the_update_row_and_check_now_asks_again() {
    let fake = Fake::new();
    *fake.update.lock().expect("lock") = UpdateState::Failed {
        detail: "timed out".into(),
    };
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::UpdateNow);
    shows(&mut app, "Could not check: timed out");
    press(&mut app, KeyCode::Enter);
    assert_eq!(*fake.update_checks.lock().expect("lock"), 1);
}

#[test]
fn the_upgrade_key_runs_brew_and_asks_for_a_restart() {
    let fake = Fake::new();
    *fake.update.lock().expect("lock") = available(UpdateVia::Homebrew);
    let (mut app, _) = app(&fake, "en");
    ch(&mut app, 'u');
    shows(&mut app, "Upgrading to Anya 0.9.0");
    app.settle();
    shows(&mut app, "Upgraded to Anya 0.9.0; restart Anya to use it.");
    assert!(!screen(&mut app, 160, 60).contains("Press u"));
}

#[test]
fn a_refused_upgrade_says_why_in_each_locale() {
    for (locale, issue, text) in [
        (
            "en",
            UpdateIssue::NotHomebrew,
            "This Anya was not installed with Homebrew, so it cannot upgrade itself.",
        ),
        (
            "en",
            UpdateIssue::BrewMissing,
            "Homebrew was not found on this computer.",
        ),
        (
            "en",
            UpdateIssue::NothingToInstall,
            "There is no newer version to install.",
        ),
        (
            "en",
            UpdateIssue::Failed {
                detail: "Error: locked".into(),
            },
            "The upgrade failed: Error: locked",
        ),
        (
            "zh-CN",
            UpdateIssue::BrewMissing,
            "在这台电脑上找不到 Homebrew。",
        ),
        (
            "zh-CN",
            UpdateIssue::Failed {
                detail: "Error: locked".into(),
            },
            "升级失败：Error: locked",
        ),
    ] {
        let fake = Fake::new();
        *fake.update.lock().expect("lock") = available(UpdateVia::Homebrew);
        *fake.upgrade_result.lock().expect("lock") = Err(issue);
        let (mut app, _) = app(&fake, locale);
        ch(&mut app, 'u');
        app.settle();
        shows(&mut app, text);
    }
}

#[test]
fn the_update_check_switch_saves_and_says_what_it_sends() {
    let fake = Fake::new();
    let (mut app, _) = app(&fake, "en");
    goto(&mut app, Field::UpdateCheck);
    shows(&mut app, "Check for updates");
    shows(&mut app, "The request carries only Anya's");
    ch(&mut app, ' ');
    ch(&mut app, 's');
    let saves = fake.saves();
    assert!(!saves[0].updates.check);
}

#[test]
fn the_recording_line_shows_the_input_level_and_the_cursor_row_is_tinted() {
    use ratatui::style::Color;
    let fake = Fake::new();
    {
        let mut status = fake.status.lock().expect("lock");
        status.dictation = Dictation::Recording;
        status.level = Some(0.5);
    }
    let (mut app, _) = app(&fake, "en");
    shows(&mut app, "██████░░░░░░");
    status_level(&fake, None);
    app.refresh();
    assert!(!screen(&mut app, 160, 60).contains('█'), "no level, no bar");

    goto(&mut app, Field::Mode);
    let mut terminal = Terminal::new(TestBackend::new(160, 60)).expect("terminal");
    terminal
        .draw(|frame| view::draw(&mut app, frame))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let tinted = |text: &str| {
        (0..60).any(|y| {
            let row: String = (0..160)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect();
            row.contains(text)
                && (0..160).any(|x| {
                    buffer
                        .cell((x, y))
                        .is_some_and(|c| c.bg == Color::Indexed(237))
                })
        })
    };
    assert!(tinted("Mode"), "the cursor's row has the tint");
    assert!(!tinted("Hotkeys"), "other rows do not");
}

fn status_level(fake: &Fake, level: Option<f32>) {
    fake.status.lock().expect("lock").level = level;
}
