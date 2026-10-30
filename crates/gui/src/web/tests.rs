//! The page server as a browser and the page see it: raw HTTP against a
//! loopback listener, a scripted [`FakeControl`] behind it.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anya_core::audio::{Device, DeviceKind};
use anya_protocol::{ErrorCode, Scope, Source};
use anya_runtime::{
    Dictation, Entry, Lan, Notice, PairedClient, SaveError, Subscription, UpdateIssue, UpdateState,
    UpdateVia,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::fake::FakeControl;
use super::*;
use crate::{Shell, ShellKind};

/// A shell that records what the page asked of it.
struct FakeShell {
    kind: ShellKind,
    opened: Mutex<Vec<String>>,
    pick: Mutex<Result<Option<PathBuf>, String>>,
    pick_start: Mutex<Option<Option<PathBuf>>>,
    settings: AtomicUsize,
    quits: AtomicUsize,
}

impl FakeShell {
    fn new(kind: ShellKind) -> FakeShell {
        FakeShell {
            kind,
            opened: Mutex::new(Vec::new()),
            pick: Mutex::new(Ok(None)),
            pick_start: Mutex::new(None),
            settings: AtomicUsize::new(0),
            quits: AtomicUsize::new(0),
        }
    }
}

impl Shell for FakeShell {
    fn kind(&self) -> ShellKind {
        self.kind
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        lock(&self.opened).push(url.to_owned());
        Ok(())
    }

    fn pick_file(&self, start: Option<&Path>) -> Result<Option<PathBuf>, String> {
        *lock(&self.pick_start) = Some(start.map(Path::to_path_buf));
        lock(&self.pick).clone()
    }

    fn show_settings(&self) {
        self.settings.fetch_add(1, Ordering::SeqCst);
    }

    fn quit(&self) {
        self.quits.fetch_add(1, Ordering::SeqCst);
    }
}

struct Res {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Res {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("a JSON body")
    }
}

/// Sends `request` as written and reads the whole answer.
async fn raw(addr: SocketAddr, request: &str) -> Res {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.expect("read answer");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("a full answer");
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .expect("a status line");
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Res {
        status,
        headers,
        body: body.to_owned(),
    }
}

fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> String {
    let mut out = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    if method != "GET" {
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out
}

struct Harness {
    server: WebServer,
    control: Arc<FakeControl>,
    shell: Arc<FakeShell>,
}

async fn harness_with(control: FakeControl, kind: ShellKind) -> Harness {
    let control = Arc::new(control);
    let shell = Arc::new(FakeShell::new(kind));
    let server = start(
        control.clone(),
        shell.clone(),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        HEADER_TIMEOUT,
    )
    .await
    .expect("serve");
    Harness {
        server,
        control,
        shell,
    }
}

async fn harness(kind: ShellKind) -> Harness {
    harness_with(FakeControl::sample(), kind).await
}

impl Harness {
    fn host(&self) -> String {
        self.server.addr().to_string()
    }

    fn key(&self) -> String {
        self.server.key.clone()
    }

    fn cookie(&self) -> String {
        format!("anya_key_{}={}", self.server.addr().port(), self.key())
    }

    async fn get(&self, path: &str) -> Res {
        let (host, cookie) = (self.host(), self.cookie());
        raw(
            self.server.addr(),
            &request("GET", path, &[("Host", &host), ("Cookie", &cookie)], ""),
        )
        .await
    }

    async fn post(&self, path: &str, body: &Value) -> Res {
        self.post_raw(path, &body.to_string()).await
    }

    async fn post_raw(&self, path: &str, body: &str) -> Res {
        let (host, cookie) = (self.host(), self.cookie());
        let origin = format!("http://{host}");
        raw(
            self.server.addr(),
            &request(
                "POST",
                path,
                &[
                    ("Host", &host),
                    ("Cookie", &cookie),
                    ("Origin", &origin),
                    ("Content-Type", "application/json"),
                ],
                body,
            ),
        )
        .await
    }
}

fn en(key: &str) -> String {
    Catalog::new("en").t(key).to_owned()
}

fn zh(key: &str) -> String {
    Catalog::new("zh-CN").t(key).to_owned()
}

// ---- who may ask ----

#[tokio::test]
async fn the_key_signs_in_with_a_strict_cookie_and_redirects_to_the_same_path() {
    let h = harness(ShellKind::Web).await;
    assert_eq!(
        h.server.url(),
        format!("http://{}/?key={}", h.host(), h.key())
    );
    assert_eq!(h.key().len(), 32);
    let host = h.host();
    for (path, location) in [
        (format!("/?key={}", h.key()), "/"),
        (format!("/panel.html?key={}", h.key()), "/panel.html"),
        // `//elsewhere` would leave the origin.
        (format!("//evil.example?key={}", h.key()), "/"),
    ] {
        let res = raw(
            h.server.addr(),
            &request("GET", &path, &[("Host", &host)], ""),
        )
        .await;
        assert_eq!(res.status, 303, "{path}");
        assert_eq!(res.header("Location"), Some(location));
        let cookie = res.header("Set-Cookie").expect("a cookie");
        assert!(cookie.starts_with(&h.cookie()), "{cookie}");
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
    }
    // With the cookie the page loads.
    let page = h.get("/").await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("app.js"));
}

#[tokio::test]
async fn a_wrong_key_or_no_cookie_is_refused_in_the_ui_language() {
    let h = harness(ShellKind::Web).await;
    let host = h.host();
    let addr = h.server.addr();
    let wrong = raw(
        addr,
        &request(
            "GET",
            "/?key=00000000000000000000000000000000",
            &[("Host", &host)],
            "",
        ),
    )
    .await;
    assert_eq!(wrong.status, 401);
    assert_eq!(wrong.body, en("gui.web.link_expired"));

    let none = raw(addr, &request("GET", "/api/state", &[("Host", &host)], "")).await;
    assert_eq!(none.status, 401);
    assert_eq!(none.body, en("gui.web.sign_in"));
    assert!(
        none.header("Content-Type")
            .is_some_and(|t| t.starts_with("text/plain"))
    );

    let other_port = format!("anya_key_1={}", h.key());
    let bad_cookie = raw(
        addr,
        &request(
            "GET",
            "/api/state",
            &[
                ("Host", &host),
                ("Cookie", "anya_key_0=nope"),
                ("Cookie", &other_port),
            ],
            "",
        ),
    )
    .await;
    assert_eq!(bad_cookie.status, 401);

    lock(&h.control.config).ui.language = "zh-CN".to_owned();
    let none = raw(addr, &request("GET", "/", &[("Host", &host)], "")).await;
    assert_eq!(none.body, zh("gui.web.sign_in"));
    assert_eq!(none.body, "请使用本次启动时输出的链接打开。");
}

#[tokio::test]
async fn host_must_be_the_listen_address() {
    let h = harness(ShellKind::Web).await;
    let cookie = h.cookie();
    let port = h.server.addr().port();
    for host in [
        format!("localhost:{port}"),
        "evil.example".to_owned(),
        String::new(),
    ] {
        let res = raw(
            h.server.addr(),
            &request(
                "GET",
                "/api/state",
                &[("Host", &host), ("Cookie", &cookie)],
                "",
            ),
        )
        .await;
        assert_eq!(res.status, 403, "{host}");
        assert_eq!(res.body, en("gui.web.forbidden"));
    }
}

#[tokio::test]
async fn posts_need_this_origin() {
    let h = harness(ShellKind::Web).await;
    let (host, cookie) = (h.host(), h.cookie());
    let body = r#"{"on":true}"#;
    for origin in [None, Some("http://evil.example"), Some("null")] {
        let mut headers = vec![("Host", host.as_str()), ("Cookie", cookie.as_str())];
        if let Some(o) = origin {
            headers.push(("Origin", o));
        }
        let res = raw(
            h.server.addr(),
            &request("POST", "/api/continuous", &headers, body),
        )
        .await;
        assert_eq!(res.status, 403, "{origin:?}");
    }
    assert!(h.control.calls().is_empty());
    assert_eq!(h.post_raw("/api/continuous", body).await.status, 200);
}

#[tokio::test]
async fn bodies_are_capped() {
    let h = harness(ShellKind::Web).await;
    let big = format!(r#"{{"on":true,"pad":"{}"}}"#, "x".repeat(70 * 1024));
    let res = h.post_raw("/api/dictation", &big).await;
    assert_eq!(res.status, 413);
    assert_eq!(res.json()["error"], en("gui.api.too_large"));
    assert!(h.control.calls().is_empty());
}

#[tokio::test]
async fn slow_headers_are_cut_off() {
    let control: Arc<dyn Control> = Arc::new(FakeControl::sample());
    let server = start(
        control,
        Arc::new(FakeShell::new(ShellKind::Web)),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        Duration::from_millis(200),
    )
    .await
    .expect("serve");
    let mut stream = TcpStream::connect(server.addr()).await.expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\n")
        .await
        .expect("write");
    let mut buf = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
    assert!(read.is_ok(), "the connection stayed open");
}

#[tokio::test]
async fn stop_closes_the_listener() {
    let h = harness(ShellKind::Web).await;
    let addr = h.server.addr();
    assert_eq!(h.get("/api/state").await.status, 200);
    h.server.stop().await;
    assert!(TcpStream::connect(addr).await.is_err());
}

// ---- static files and the catalog ----

#[tokio::test]
async fn the_files_are_served_with_their_types_and_a_strict_policy() {
    let h = harness(ShellKind::Web).await;
    for (path, ty) in [
        ("/", "text/html"),
        ("/panel.html", "text/html"),
        ("/app.css", "text/css"),
        ("/common.js", "text/javascript"),
        ("/app.js", "text/javascript"),
        ("/panel.js", "text/javascript"),
        ("/icon.svg", "image/svg+xml"),
    ] {
        let res = h.get(path).await;
        assert_eq!(res.status, 200, "{path}");
        assert!(
            res.header("Content-Type")
                .is_some_and(|t| t.starts_with(ty)),
            "{path}"
        );
        assert_eq!(res.header("Cache-Control"), Some("no-cache"));
        assert!(
            res.header("Content-Security-Policy")
                .is_some_and(|c| c.contains("script-src 'self'"))
        );
    }
    let missing = h.get("/nothing.js").await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.body, en("gui.web.not_found"));
    let api = h.get("/api/nothing").await;
    assert_eq!(api.status, 404);
    assert_eq!(api.json()["error"], en("gui.api.unsupported"));
}

#[tokio::test]
async fn i18n_answers_the_whole_catalog_in_the_ui_language() {
    let h = harness(ShellKind::Web).await;
    let res = h.get("/api/i18n").await.json();
    assert_eq!(res["locale"], "en");
    assert_eq!(res["text"]["gui.nav.engine"], "Engine");
    assert_eq!(res["text"]["common.cancel"], "Cancel");
    lock(&h.control.config).ui.language = "zh-CN".to_owned();
    let res = h.get("/api/i18n").await.json();
    assert_eq!(res["locale"], "zh-CN");
    assert_eq!(res["text"]["gui.nav.engine"], "引擎");
}

// ---- state and status ----

#[tokio::test]
async fn route_translates_where_sessions_come_from_and_go_and_splits_a_final_by_destination() {
    use anya_protocol::SessionId;
    use anya_runtime::{LiveRoute, Origin, Route, RouteEvent, Sink, Step};
    let control = FakeControl::sample();
    lock(&control.config).ui.language = "zh-CN".to_owned();
    let phone = Origin::Remote {
        client: "iPhone".to_owned(),
    };
    let sinks = vec![
        Sink::Client {
            name: "iPhone".to_owned(),
        },
        Sink::Subscribers { count: 2 },
    ];
    let event = |seq, origin: &Origin, step| RouteEvent {
        seq,
        at: 1,
        session: SessionId(format!("s{seq}")),
        origin: origin.clone(),
        engine: "cloud".to_owned(),
        step,
    };
    *lock(&control.route) = Route {
        live: vec![LiveRoute {
            session: SessionId("s1".to_owned()),
            origin: phone.clone(),
            engine: "cloud".to_owned(),
            streaming: true,
            level: 0.5,
            text: "明天".to_owned(),
            sinks: sinks.clone(),
        }],
        log: vec![
            event(1, &phone, Step::Audio { ms: 1000 }),
            event(
                2,
                &phone,
                Step::Final {
                    text: "明天三点".to_owned(),
                    sinks,
                },
            ),
            event(
                3,
                &Origin::Dictation { device: None },
                Step::Final {
                    text: "好的".to_owned(),
                    sinks: vec![Sink::Paste],
                },
            ),
        ],
    };
    let h = harness_with(control, ShellKind::App).await;
    let r = h.get("/api/route").await.json();

    let live = &r["live"][0];
    assert_eq!(live["origin"]["label"], "iPhone");
    assert_eq!(live["origin"]["role"], "远程听写");
    assert_eq!(live["engine"]["title"], "云端流式");
    assert_eq!(live["sinks"][0]["label"], "回到 iPhone");
    assert_eq!(live["sinks"][1]["label"], "订阅的应用 ×2");

    let log = r["log"].as_array().expect("log");
    let lines: Vec<(&str, &str, &str, &str)> = log
        .iter()
        .map(|l| {
            let s = |k: &str| l[k].as_str().unwrap_or_default();
            (s("from"), s("to"), s("label"), s("detail"))
        })
        .collect();
    assert_eq!(
        lines,
        [
            ("iPhone", "云端流式", "音频", "1.0 秒"),
            ("云端流式", "回到 iPhone", "定稿", "明天三点"),
            ("云端流式", "订阅的应用 ×2", "定稿", "已授权 · 2 个"),
            ("云端流式", "上屏", "定稿", "好的"),
        ]
    );
    let activity: Vec<bool> = log.iter().map(|l| l["activity"] == true).collect();
    assert_eq!(
        activity,
        [false, false, false, true],
        "only the user's own dictation is on the Activity page"
    );
}

#[tokio::test]
async fn state_carries_the_forms_choices_translated() {
    let control = FakeControl::sample();
    {
        let mut c = lock(&control.config);
        c.dictation.hotkeys = "super+alt, f13".to_owned();
        c.dictation.device = "Old USB Mic".to_owned();
        c.recognition.language = "pt-BR".to_owned();
        c.overlay.position = "middle".to_owned();
        c.recognition.engines.insert(
            "cloud".to_owned(),
            BTreeMap::from([("api_key".to_owned(), "k".to_owned())]),
        );
    }
    let h = harness_with(control, ShellKind::App).await;
    let s = h.get("/api/state").await.json();
    assert_eq!(s["shell"], "app");
    assert_eq!(s["locale"], "en");
    // Canonical hotkeys, so an unchanged list compares equal.
    assert_eq!(s["config"]["dictation"]["hotkeys"], "Alt+Super, F13");
    // Kept as configured even though it is not plugged in.
    assert_eq!(s["config"]["dictation"]["device"], "Old USB Mic");
    assert_eq!(s["devices"][0]["kind"], "microphone");
    assert_eq!(s["devices"][0]["id"], "BuiltInMic");

    let engines = s["engines"].as_array().expect("engines");
    assert_eq!(engines[0]["id"], "cloud");
    assert_eq!(engines[0]["title"], "Cloud Stream");
    assert_eq!(engines[0]["ready"], true);
    assert_eq!(engines[0]["streaming"], true);
    assert_eq!(engines[0]["fields"][0]["kind"], "secret");
    assert_eq!(engines[0]["fields"][0]["required"], true);
    assert_eq!(engines[0]["fields"][1]["kind"], "choice");
    assert_eq!(engines[0]["fields"][1]["options"], json!(["v1", "v2"]));
    assert_eq!(engines[0]["fields"][1]["advanced"], true);
    assert_eq!(engines[1]["id"], "whisper");
    assert_eq!(engines[1]["ready"], true);
    assert_eq!(engines[1]["plugin"], false);
    assert_eq!(engines[2]["id"], "sherpa");
    assert_eq!(engines[2]["plugin"], true);
    assert_eq!(engines[2]["ready"], true);

    let positions = s["positions"].as_array().expect("positions");
    assert_eq!(positions[0], json!({"value": "notch", "label": "Notch"}));
    assert_eq!(
        positions.last(),
        Some(&json!({"value": "middle", "label": "middle"}))
    );
    let languages = s["languages"].as_array().expect("languages");
    assert_eq!(languages[0], json!({"value": "", "label": "Auto"}));
    assert_eq!(
        languages[1],
        json!({"value": "zh-CN", "label": "简体中文 (zh-CN)"})
    );
    assert_eq!(
        languages.last(),
        Some(&json!({"value": "pt-BR", "label": "pt-BR"}))
    );
    // The first choice names the language the system would give.
    let ui = s["ui_languages"].as_array().expect("ui languages");
    assert_eq!(ui[0]["value"], "system");
    assert!(
        ui[0]["label"]
            .as_str()
            .is_some_and(|l| l.starts_with("Follow the system (")),
        "{}",
        ui[0]
    );
    assert_eq!(
        ui[1..],
        [
            json!({"value": "en", "label": "English"}),
            json!({"value": "zh-CN", "label": "简体中文"}),
        ]
    );
}

#[tokio::test]
async fn state_in_chinese_and_with_an_engine_anya_lacks() {
    let control = FakeControl::sample();
    {
        let mut c = lock(&control.config);
        c.ui.language = "zh-CN".to_owned();
        c.recognition.dictation = "  ".to_owned();
        c.recognition.continuous = "  ".to_owned();
        c.recognition.remote = "whisper".to_owned();
    }
    *lock(&control.devices) = Err("no audio server".to_owned());
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/state").await.json();
    assert_eq!(s["locale"], "zh-CN");
    assert_eq!(s["engines"][0]["title"], "云端流式");
    assert_eq!(s["engines"][0]["ready"], false);
    // A blank engine is the default one, shown although Anya lacks it.
    let current = s["config"]["recognition"]["dictation"].clone();
    assert_eq!(current, anya_config::DEFAULT_ENGINE);
    assert_eq!(s["config"]["recognition"]["continuous"], current);
    assert_eq!(s["config"]["recognition"]["remote"], "whisper");
    let custom = s["engines"]
        .as_array()
        .and_then(|e| e.last())
        .expect("custom card");
    assert_eq!(custom["id"], current);
    assert_eq!(custom["custom"], true);
    assert_eq!(custom["blurb"], "自定义引擎");
    assert_eq!(s["devices"], json!([]));
    assert_eq!(s["devices_error"], "无法列出录音设备：no audio server");
    assert_eq!(s["languages"][1]["label"], "简体中文（zh-CN）");
}

#[tokio::test]
async fn status_reports_state_activity_and_subscription_translated() {
    let control = FakeControl::sample();
    {
        let mut st = lock(&control.status);
        st.dictation = Dictation::Failed {
            error: ErrorCode::Connect,
            detail: Some("connection refused".to_owned()),
        };
        st.finishing = 1;
    }
    lock(&control.activity).push(Entry::Notice {
        at: 5,
        notice: Notice::HotwordsUnreadable,
        detail: Some("~/words.txt".to_owned()),
    });
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/status").await.json();
    assert_eq!(
        s["pill"],
        json!({"state": "failed", "label": "Something went wrong"})
    );
    assert_eq!(
        s["failure"],
        json!({"text": "Cannot reach the recognition service", "detail": "connection refused"})
    );
    assert_eq!(
        s["detail"],
        "Cannot reach the recognition service: connection refused  1 finishing in the background"
    );
    assert_eq!(s["subscription"]["state"], "ok");
    assert_eq!(s["subscription"]["line"], "Listening · 2 apps connected");
    assert_eq!(s["subscription"]["url"], "ws://127.0.0.1:7590/asr");
    assert_eq!(s["subscription"]["subscribers"], 2);
    assert_eq!(s["stats"]["sessions"], 128);
    assert_eq!(s["status"]["level"], Value::Null);
    assert_eq!(s["status"]["finishing"], 1);
    let activity = s["activity"].as_array().expect("activity");
    assert_eq!(activity[0]["text"], "Settings file changed; applied");
    assert_eq!(activity[1]["kind"], "final");
    assert_eq!(activity[1]["note"], "Pasted");
    assert_eq!(activity[2]["level"], "error");
    assert_eq!(activity[2]["text"], "Cannot reach the recognition service");
    assert_eq!(activity[2]["detail"], "connection refused");
    assert_eq!(activity[3]["source"], "continuous");
    assert_eq!(activity[3]["note"], "Transcribed");
    assert_eq!(activity[4]["level"], "warn");
    assert_eq!(
        activity[4]["text"],
        "Cannot read the hotwords file: ~/words.txt"
    );

    // Down, idle with dictation off, in Chinese.
    {
        let mut st = lock(&h.control.status);
        st.dictation = Dictation::Idle;
        st.finishing = 0;
        st.subscription = Subscription::Down {
            detail: "address in use".to_owned(),
        };
    }
    {
        let mut c = lock(&h.control.config);
        c.ui.language = "zh-CN".to_owned();
        c.dictation.enabled = false;
        c.recognition.engines.insert(
            "cloud".to_owned(),
            BTreeMap::from([("api_key".to_owned(), "k".to_owned())]),
        );
    }
    let s = h.get("/api/status").await.json();
    assert_eq!(s["pill"], json!({"state": "off", "label": "听写已关闭"}));
    assert_eq!(s["detail"], "");
    assert_eq!(
        s["subscription"],
        json!({"state": "missing", "line": "未启动：address in use"})
    );
    assert_eq!(s["activity"][1]["note"], "已上屏");
    assert_eq!(s["failure"], Value::Null);
}

#[tokio::test]
async fn debug_lines_show_only_while_debug_mode_is_on() {
    let control = FakeControl::sample();
    lock(&control.activity).extend([
        Entry::Debug {
            at: 10,
            event: anya_runtime::DebugEvent::KeyDown {
                combo: "Fn".to_owned(),
            },
        },
        Entry::Debug {
            at: 11,
            event: anya_runtime::DebugEvent::Timing {
                connect_ms: Some(210),
                first_text_ms: Some(830),
                final_ms: Some(480),
            },
        },
    ]);
    let h = harness_with(control, ShellKind::Web).await;
    let shown = |s: &Value| {
        s["activity"]
            .as_array()
            .map_or(0, |a| a.iter().filter(|e| e["kind"] == "debug").count())
    };
    assert_eq!(shown(&h.get("/api/status").await.json()), 0);

    let mut c = edited(&h);
    c["debug"]["enabled"] = json!(true);
    assert_eq!(h.post("/api/settings", &c).await.status, 200);
    let s = h.get("/api/status").await.json();
    assert_eq!(shown(&s), 2);
    let lines: Vec<&str> = s["activity"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == "debug")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert_eq!(
        lines,
        [
            "Pressed Fn",
            "Connected 210 ms · First words 830 ms · Stop to result 480 ms"
        ]
    );

    c["debug"]["enabled"] = json!(false);
    h.post("/api/settings", &c).await;
    assert_eq!(shown(&h.get("/api/status").await.json()), 0);
}

#[tokio::test]
async fn status_shows_only_the_latest_hundred_entries() {
    let control = FakeControl::sample();
    *lock(&control.activity) = (0..150)
        .map(|i| Entry::Final {
            at: i,
            source: Source::Dictation,
            text: format!("line {i}"),
            delivered: anya_runtime::Delivered::Copied,
            session: None,
            engine: None,
        })
        .collect();
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/status").await.json();
    let activity = s["activity"].as_array().expect("activity");
    assert_eq!(activity.len(), 100);
    assert_eq!(activity[0]["text"], "line 50");
    assert_eq!(activity[99]["note"], "Copied");
}

// ---- saving ----

fn edited(h: &Harness) -> Value {
    serde_json::to_value(h.control.config()).expect("config as JSON")
}

#[tokio::test]
async fn save_normalizes_and_answers_the_new_state() {
    let h = harness(ShellKind::Web).await;
    let mut c = edited(&h);
    c["dictation"]["device"] = json!("  BuiltInMic ");
    c["recognition"]["hotwords"] = json!(["Anya, 雷达", "Anya", " 雷达 ", "Spy、Family"]);
    c["recognition"]["engines"] = json!({"cloud": {"api_key": " sk-1 ", "region": "  "}});
    c["subscription"]["address"] = json!(" ");
    c["ui"]["language"] = json!("zh-CN");
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let saved = lock(&h.control.saved).last().cloned().expect("a save");
    assert_eq!(saved.dictation.device, "BuiltInMic");
    assert_eq!(
        saved.recognition.hotwords,
        ["Anya", "雷达", "Spy", "Family"]
    );
    assert_eq!(
        saved.recognition.engines["cloud"],
        BTreeMap::from([("api_key".to_owned(), "sk-1".to_owned())])
    );
    assert_eq!(saved.subscription.address, anya_config::DEFAULT_ADDRESS);
    // The answer speaks the language just saved.
    let body = res.json();
    assert_eq!(body["state"]["locale"], "zh-CN");
    assert_eq!(body["state"]["engines"][0]["ready"], true);
    assert!(body.get("warning").is_none());
}

#[tokio::test]
async fn save_warns_when_the_hotkeys_did_not_apply() {
    let control = FakeControl::sample();
    *lock(&control.hotkeys_failed) = Some("Accessibility denied".to_owned());
    let h = harness_with(control, ShellKind::Web).await;
    let res = h.post("/api/settings", &edited(&h)).await;
    assert_eq!(res.status, 200);
    assert_eq!(
        res.json()["warning"],
        "Hotkeys could not be registered: Accessibility denied"
    );
}

#[tokio::test]
async fn each_use_saves_its_own_engine_and_is_refused_by_its_own_field() {
    let h = harness(ShellKind::Web).await;
    let mut c = edited(&h);
    c["recognition"]["dictation"] = json!("whisper");
    c["recognition"]["continuous"] = json!("sherpa");
    c["recognition"]["remote"] = json!("cloud");
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let saved = lock(&h.control.saved).last().cloned().expect("a save");
    assert_eq!(saved.recognition.dictation, "whisper");
    assert_eq!(saved.recognition.continuous, "sherpa");
    assert_eq!(saved.recognition.remote, "cloud");
    let config = &res.json()["state"]["config"]["recognition"];
    assert_eq!(config["dictation"], "whisper");
    assert_eq!(config["continuous"], "sherpa");
    assert_eq!(config["remote"], "cloud");

    for role in ["dictation", "continuous", "remote"] {
        let mut c = edited(&h);
        c["recognition"][role] = json!("nope");
        let res = h.post("/api/settings", &c).await;
        assert_eq!(res.status, 400, "{role}: {}", res.body);
        assert_eq!(
            res.json()["invalid"],
            json!([{
                "fields": [format!("recognition.{role}")],
                "message": "Unknown recognition engine: nope",
            }]),
            "{role}"
        );
    }
}

#[tokio::test]
async fn invalid_settings_are_refused_per_field_in_both_languages() {
    let h = harness(ShellKind::Web).await;
    let mut c = edited(&h);
    c["dictation"]["hotkeys"] = json!("Ctrl+A");
    c["recognition"]["continuous"] = json!("nope");
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 400);
    let body = res.json();
    assert_eq!(
        body["invalid"],
        json!([
            {
                "fields": ["dictation.hotkeys"],
                "message": "Ctrl+A types text; use a combination such as Alt+Super, F9, Alt+F8 or Ctrl+Alt+Tab",
            },
            {"fields": ["recognition.continuous"], "message": "Unknown recognition engine: nope"},
        ])
    );
    assert_eq!(
        body["error"],
        "Not saved: Ctrl+A types text; use a combination such as Alt+Super, F9, Alt+F8 or Ctrl+Alt+Tab; Unknown recognition engine: nope"
    );
    assert!(lock(&h.control.saved).is_empty());

    // The continuous switch reusing a dictation hotkey, in Chinese.
    lock(&h.control.config).ui.language = "zh-CN".to_owned();
    let mut c = edited(&h);
    c["continuous"]["hotkey"] = json!("f13");
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 400);
    let body = res.json();
    assert_eq!(
        body["invalid"],
        json!([{"fields": ["continuous.hotkey"], "message": "F13 已经是听写热键，不能再用来开关持续转写"}])
    );
    assert_eq!(
        body["error"],
        "未保存：F13 已经是听写热键，不能再用来开关持续转写"
    );

    let mut c = edited(&h);
    c["subscription"]["address"] = json!("0.0.0.0:7590");
    let res = h.post("/api/settings", &c).await.json();
    assert_eq!(res["invalid"][0]["fields"], json!(["subscription.address"]));
    assert_eq!(
        res["invalid"][0]["message"],
        "无法在 0.0.0.0:7590 上监听：not a loopback address"
    );
    assert!(lock(&h.control.saved).is_empty());
}

#[tokio::test]
async fn a_failed_write_and_a_malformed_body_are_errors() {
    let control = FakeControl::sample();
    *lock(&control.save_error) = Some(SaveError::Write("disk full".to_owned()));
    let h = harness_with(control, ShellKind::Web).await;
    let res = h.post("/api/settings", &edited(&h)).await;
    assert_eq!(res.status, 500);
    assert_eq!(res.json()["error"], "Saving failed: disk full");

    for body in [
        "{",
        r#"{"voice": {}}"#,
        r#"{"dictation": {"stop_delay_ms": -5}}"#,
    ] {
        let res = h.post_raw("/api/settings", body).await;
        assert_eq!(res.status, 400, "{body}");
        assert_eq!(res.json()["error"], en("gui.api.bad_request"));
    }
}

// ---- switches ----

#[tokio::test]
async fn the_continuous_switch_calls_set_continuous() {
    let h = harness(ShellKind::App).await;
    let res = h.post("/api/continuous", &json!({"on": true})).await;
    assert_eq!(res.status, 200);
    assert_eq!(h.control.calls(), ["set_continuous true"]);
    assert_eq!(res.json()["state"]["config"]["continuous"]["enabled"], true);
    let s = h.get("/api/status").await.json();
    assert_eq!(s["status"]["continuous_on"], true);
    let last = s["activity"].as_array().and_then(|a| a.last()).cloned();
    assert_eq!(
        last.map(|e| e["text"].clone()),
        Some(json!("Continuous transcription on"))
    );

    h.post("/api/continuous", &json!({"on": false})).await;
    assert_eq!(
        h.control.calls(),
        ["set_continuous true", "set_continuous false"]
    );
    assert_eq!(
        h.get("/api/status").await.json()["status"]["continuous_on"],
        false
    );
}

#[tokio::test]
async fn the_dictation_switch_calls_set_dictation() {
    let h = harness(ShellKind::App).await;
    let res = h.post("/api/dictation", &json!({"on": false})).await;
    assert_eq!(res.status, 200);
    assert_eq!(h.control.calls(), ["set_dictation false"]);
    assert_eq!(res.json()["state"]["config"]["dictation"]["enabled"], false);
    assert_eq!(h.post_raw("/api/dictation", "{}").await.status, 400);
}

// ---- hotkeys ----

#[tokio::test]
async fn check_answers_the_canonical_name_or_the_reason() {
    let h = harness(ShellKind::Web).await;
    let ok = h
        .post(
            "/api/hotkey/check",
            &json!({"value": " super+alt ", "role": "dictation"}),
        )
        .await;
    assert_eq!(ok.status, 200);
    assert_eq!(ok.json()["value"], "Alt+Super");
    for (value, message) in [
        (
            "Shift+Space",
            "Shift+Space types text; use a combination such as Alt+Super, F9, Alt+F8 or Ctrl+Alt+Tab",
        ),
        ("Fn+F5", "Fn cannot be combined with F1–F24: Fn+F5"),
        ("Hyper", "Cannot read the hotkey \"Hyper\""),
    ] {
        let res = h
            .post(
                "/api/hotkey/check",
                &json!({"value": value, "role": "dictation"}),
            )
            .await;
        assert_eq!(res.status, 400, "{value}");
        assert_eq!(res.json()["error"], message);
    }
    let taken = h
        .post(
            "/api/hotkey/check",
            &json!({"value": "F13", "role": "continuous"}),
        )
        .await;
    assert_eq!(taken.status, 400);
}

#[tokio::test]
async fn record_asks_the_os_one_at_a_time() {
    let h = harness(ShellKind::App).await;
    let res = h.post("/api/hotkey/record", &json!({})).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["value"], "F9");
    assert_eq!(h.control.calls(), ["record_hotkey 15s"]);

    *lock(&h.control.record) = Ok(None);
    assert_eq!(
        h.post("/api/hotkey/record", &json!({})).await.json()["value"],
        Value::Null
    );

    *lock(&h.control.record_delay) = Duration::from_millis(600);
    let h = Arc::new(h);
    let first = tokio::spawn({
        let h = h.clone();
        async move { h.post("/api/hotkey/record", &json!({})).await.status }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let second = h.post("/api/hotkey/record", &json!({})).await;
    assert_eq!(second.status, 409);
    assert_eq!(second.json()["error"], en("gui.api.record_busy"));
    assert_eq!(first.await.expect("first request"), 200);

    *lock(&h.control.record_delay) = Duration::ZERO;
    *lock(&h.control.record) = Err("no event tap".to_owned());
    let failed = h.post("/api/hotkey/record", &json!({})).await;
    assert_eq!(failed.status, 500);
    assert_eq!(
        failed.json()["error"],
        "Cannot record the hotkey: no event tap"
    );
}

#[tokio::test]
async fn suspend_pauses_hotkeys_until_resumed_or_saved() {
    let h = harness(ShellKind::Web).await;
    assert_eq!(
        h.post("/api/hotkey/suspend", &json!({"active": true}))
            .await
            .status,
        204
    );
    assert_eq!(
        h.post("/api/hotkey/suspend", &json!({"active": false}))
            .await
            .status,
        204
    );
    // Nothing paused: resuming again does not touch the hotkeys.
    h.post("/api/hotkey/suspend", &json!({"active": false}))
        .await;
    assert_eq!(
        h.control.calls(),
        ["suspend_hotkeys Some(15s)", "suspend_hotkeys None"]
    );

    h.post("/api/hotkey/suspend", &json!({"active": true}))
        .await;
    h.post("/api/settings", &edited(&h)).await;
    assert_eq!(
        h.control.calls()[2..],
        ["suspend_hotkeys Some(15s)", "save", "suspend_hotkeys None"]
    );
}

// ---- what only the app shell does ----

#[tokio::test]
async fn open_takes_engine_docs_in_the_app_only() {
    let h = harness(ShellKind::App).await;
    let ok = h
        .post(
            "/api/open",
            &json!({"url": "https://example.com/cloud-docs"}),
        )
        .await;
    assert_eq!(ok.status, 204);
    assert_eq!(*lock(&h.shell.opened), ["https://example.com/cloud-docs"]);
    let other = h
        .post("/api/open", &json!({"url": "file:///etc/passwd"}))
        .await;
    assert_eq!(other.status, 403);
    assert_eq!(other.json()["error"], en("gui.api.open_refused"));

    let web = harness(ShellKind::Web).await;
    let res = web
        .post(
            "/api/open",
            &json!({"url": "https://example.com/cloud-docs"}),
        )
        .await;
    assert_eq!(res.status, 403);
    assert!(lock(&web.shell.opened).is_empty());
}

#[tokio::test]
async fn pick_file_starts_next_to_the_current_file_and_answers_home_relative() {
    let h = harness(ShellKind::App).await;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let chosen = home
        .clone()
        .unwrap_or_else(|| PathBuf::from("/srv"))
        .join("notes/words.txt");
    *lock(&h.shell.pick) = Ok(Some(chosen.clone()));
    let res = h
        .post("/api/pick-file", &json!({"path": "/srv/lists/old.txt"}))
        .await;
    assert_eq!(res.status, 200);
    let want = match home {
        Some(_) => "~/notes/words.txt".to_owned(),
        None => chosen.display().to_string(),
    };
    assert_eq!(res.json()["path"], want);
    assert_eq!(
        *lock(&h.shell.pick_start),
        Some(Some(PathBuf::from("/srv/lists")))
    );

    *lock(&h.shell.pick) = Ok(None);
    assert_eq!(
        h.post("/api/pick-file", &json!({"path": ""})).await.json()["path"],
        ""
    );
    assert_eq!(*lock(&h.shell.pick_start), Some(None));

    *lock(&h.shell.pick) = Err("no portal".to_owned());
    let failed = h.post("/api/pick-file", &json!({"path": ""})).await;
    assert_eq!(failed.status, 500);
    assert_eq!(
        failed.json()["error"],
        "Cannot open the file chooser: no portal"
    );

    let web = harness(ShellKind::Web).await;
    let res = web.post("/api/pick-file", &json!({"path": ""})).await;
    assert_eq!(res.status, 404);
    assert_eq!(res.json()["error"], en("gui.api.unsupported"));
}

async fn eventually(check: impl Fn() -> bool) -> bool {
    for _ in 0..40 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test]
async fn window_actions_answer_first_then_act_in_the_app_only() {
    let h = harness(ShellKind::App).await;
    assert_eq!(
        h.post("/api/window", &json!({"action": "settings"}))
            .await
            .status,
        204
    );
    assert!(eventually(|| h.shell.settings.load(Ordering::SeqCst) == 1).await);
    assert_eq!(
        h.post("/api/window", &json!({"action": "quit"}))
            .await
            .status,
        204
    );
    assert!(eventually(|| h.shell.quits.load(Ordering::SeqCst) == 1).await);
    assert_eq!(
        h.post("/api/window", &json!({"action": "privacy"}))
            .await
            .status,
        204
    );
    assert!(eventually(|| lock(&h.shell.opened).len() == 1).await);
    assert_eq!(
        lock(&h.shell.opened)[0],
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
    );
    assert_eq!(
        h.post("/api/window", &json!({"action": "explode"}))
            .await
            .status,
        404
    );

    let web = harness(ShellKind::Web).await;
    let res = web.post("/api/window", &json!({"action": "quit"})).await;
    assert_eq!(res.status, 404);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(web.shell.quits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn copy_reaches_the_clipboard_or_says_why_not() {
    let h = harness(ShellKind::Web).await;
    assert_eq!(
        h.post("/api/copy", &json!({"text": "你好 Anya"}))
            .await
            .status,
        204
    );
    assert_eq!(*lock(&h.control.copied), ["你好 Anya"]);
    *lock(&h.control.copy_error) = Some("no clipboard".to_owned());
    let res = h.post("/api/copy", &json!({"text": "x"})).await;
    assert_eq!(res.status, 500);
    assert_eq!(res.json()["error"], "Cannot copy: no clipboard");
}

#[tokio::test]
async fn the_overlay_preview_shows_and_says_when_it_cannot() {
    let h = harness(ShellKind::App).await;
    for (view, call) in [
        ("listening", "preview_overlay Listening"),
        ("done", "preview_overlay Done"),
        ("error", "preview_overlay Error"),
    ] {
        let res = h.post("/api/overlay/preview", &json!({"view": view})).await;
        assert_eq!(res.status, 204, "{view}");
        assert_eq!(h.control.calls().last().map(String::as_str), Some(call));
    }
    assert_eq!(
        h.post("/api/overlay/preview", &json!({"view": "sideways"}))
            .await
            .status,
        400
    );

    *lock(&h.control.preview_error) = Some(anya_runtime::PreviewIssue::NotRunning);
    let res = h
        .post("/api/overlay/preview", &json!({"view": "done"}))
        .await;
    assert_eq!(res.status, 409);
    assert_eq!(
        res.json()["error"],
        en("error.preview.not_running"),
        "the refusal reads in the UI language"
    );

    let web = harness(ShellKind::Web).await;
    let res = web
        .post("/api/overlay/preview", &json!({"view": "done"}))
        .await;
    assert_eq!(res.status, 204, "a browser tab previews the same overlay");
}

#[tokio::test]
async fn a_shown_failure_can_be_retried_or_dismissed() {
    let control = FakeControl::sample();
    lock(&control.status).dictation = Dictation::Failed {
        error: ErrorCode::Connect,
        detail: None,
    };
    let h = harness_with(control, ShellKind::App).await;
    assert_eq!(
        h.post("/api/dictation/dismiss", &json!({})).await.status,
        204
    );
    assert_eq!(
        h.get("/api/status").await.json()["pill"]["state"],
        "not_ready"
    );
    lock(&h.control.status).dictation = Dictation::Failed {
        error: ErrorCode::Connect,
        detail: None,
    };
    assert_eq!(h.post("/api/dictation/retry", &json!({})).await.status, 204);
    assert_eq!(
        h.get("/api/status").await.json()["pill"]["state"],
        "recording"
    );
    assert_eq!(h.control.calls(), ["dismiss_dictation", "retry_dictation"]);
}

#[tokio::test]
async fn the_input_level_reaches_the_page_while_recording() {
    let control = FakeControl::sample();
    {
        let mut st = lock(&control.status);
        st.dictation = Dictation::Recording;
        st.level = Some(0.5);
    }
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/status").await.json();
    assert_eq!(s["pill"]["state"], "recording");
    assert_eq!(s["status"]["level"], 0.5);
}

#[tokio::test]
async fn the_status_engine_follows_the_dictation_engine_only() {
    let h = harness(ShellKind::App).await;
    let mut c = edited(&h);
    c["recognition"]["continuous"] = json!("whisper");
    c["recognition"]["remote"] = json!("sherpa");
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 200, "{}", res.body);
    // Dictation still uses `cloud`, which has no key: not ready.
    let s = h.get("/api/status").await.json();
    assert_eq!(
        s["engine"],
        json!({"id": "cloud", "title": "Cloud Stream", "ready": false})
    );
    assert_eq!(s["pill"]["state"], "not_ready");

    let mut c = edited(&h);
    c["recognition"]["dictation"] = json!("whisper");
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let s = h.get("/api/status").await.json();
    assert_eq!(
        s["engine"],
        json!({"id": "whisper", "title": "Whisper", "ready": true})
    );
    assert_eq!(s["pill"]["state"], "idle");
}

#[tokio::test]
async fn the_panel_state_is_not_ready_or_broken_before_it_is_idle() {
    let control = FakeControl::sample();
    let h = harness_with(control, ShellKind::App).await;
    let s = h.get("/api/status").await.json();
    assert_eq!(
        s["pill"],
        json!({"state": "not_ready", "label": "Not ready"})
    );
    assert_eq!(
        s["engine"],
        json!({"id": "cloud", "title": "Cloud Stream", "ready": false})
    );

    lock(&h.control.config).recognition.engines.insert(
        "cloud".to_owned(),
        BTreeMap::from([("api_key".to_owned(), "k".to_owned())]),
    );
    let s = h.get("/api/status").await.json();
    assert_eq!(s["pill"], json!({"state": "idle", "label": "Ready"}));
    assert_eq!(s["config_broken"], false);

    // The file went bad after the server started, and was not read again.
    lock(&h.control.activity).push(Entry::Notice {
        at: super::now_ms() + 1,
        notice: Notice::ConfigBroken,
        detail: Some("line 3".to_owned()),
    });
    let s = h.get("/api/status").await.json();
    assert_eq!(
        s["pill"],
        json!({"state": "config_broken", "label": "Settings file has an error"})
    );
    assert_eq!(s["config_broken"], true);
    lock(&h.control.activity).push(Entry::Notice {
        at: super::now_ms() + 2,
        notice: Notice::ConfigReloaded,
        detail: None,
    });
    assert_eq!(h.get("/api/status").await.json()["config_broken"], false);
}

fn paired(id: &str, name: &str) -> PairedClient {
    PairedClient {
        id: id.to_owned(),
        name: name.to_owned(),
        scopes: vec![Scope::Dictate],
        paired_at: 1,
        last_seen: None,
        connections: 0,
    }
}

fn listening() -> Lan {
    Lan::Listening {
        url: "wss://192.168.1.5:7591/asr".to_owned(),
        fingerprint: "ab:cd".to_owned(),
    }
}

#[tokio::test]
async fn lan_status_and_clients_reach_the_page_in_its_language() {
    let control = FakeControl::sample();
    lock(&control.clients).push(paired("c1", "Kitchen iPad"));
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/status").await.json();
    assert_eq!(
        s["lan"],
        json!({"state": "off", "line": "LAN sharing is off"})
    );
    assert_eq!(s["clients"][0]["name"], "Kitchen iPad");
    assert_eq!(s["clients"][0]["scopes"], json!(["dictate"]));

    lock(&h.control.status).lan = listening();
    let s = h.get("/api/status").await.json();
    assert_eq!(s["lan"]["state"], "ok");
    assert_eq!(s["lan"]["line"], "Listening on wss://192.168.1.5:7591/asr");
    assert_eq!(s["lan"]["fingerprint"], "ab:cd");
    assert_eq!(h.get("/api/state").await.json()["lan"], s["lan"]);

    lock(&h.control.status).lan = Lan::Down {
        detail: "address in use".to_owned(),
    };
    let s = h.get("/api/status").await.json();
    assert_eq!(s["lan"]["state"], "missing");
    assert_eq!(
        s["lan"]["line"],
        "LAN sharing is not listening: address in use"
    );
}

#[tokio::test]
async fn pairing_gives_a_code_and_a_qr_or_says_why_not() {
    let h = harness(ShellKind::Web).await;
    let res = h.post("/api/pairing", &json!({})).await;
    assert_eq!(res.status, 409);
    assert_eq!(res.json()["error"], "Cannot pair: LAN sharing is off");

    lock(&h.control.status).lan = Lan::Down {
        detail: "address in use".to_owned(),
    };
    let res = h.post("/api/pairing", &json!({})).await;
    assert_eq!(res.status, 409);
    assert!(
        res.json()["error"]
            .as_str()
            .is_some_and(|e| e.contains("address in use"))
    );

    lock(&h.control.status).lan = listening();
    let res = h.post("/api/pairing", &json!({})).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let p = res.json();
    assert_eq!(p["code"], "123456");
    assert_eq!(p["url"], "wss://192.168.1.5:7591/asr");
    assert_eq!(p["fingerprint"], "ab:cd");
    assert!(
        (1..=120).contains(&p["expires_in"].as_u64().expect("seconds")),
        "{p}"
    );
    assert!(p["qr"].as_str().is_some_and(|s| s.contains("<svg")));
    assert_eq!(h.control.calls(), ["start_pairing"; 3]);
}

#[tokio::test]
async fn client_scopes_change_and_a_bad_request_changes_nothing() {
    let control = FakeControl::sample();
    lock(&control.clients).push(paired("c1", "Kitchen iPad"));
    let h = harness_with(control, ShellKind::Web).await;
    let res = h
        .post(
            "/api/clients/scopes",
            &json!({"id": "c1", "scopes": ["subscribe_dictation", "dictate", "dictate"]}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert_eq!(
        res.json()["clients"][0]["scopes"],
        json!(["dictate", "subscribe_dictation"])
    );
    assert_eq!(
        lock(&h.control.clients)[0].scopes,
        [Scope::Dictate, Scope::SubscribeDictation]
    );

    let res = h
        .post("/api/clients/scopes", &json!({"id": "c1", "scopes": []}))
        .await;
    assert_eq!(res.json()["clients"][0]["scopes"], json!([]));

    let res = h
        .post(
            "/api/clients/scopes",
            &json!({"id": "nobody", "scopes": []}),
        )
        .await;
    assert_eq!(res.status, 404);
    assert_eq!(res.json()["error"], "That app is not paired");

    let before = h.control.calls().len();
    let res = h
        .post(
            "/api/clients/scopes",
            &json!({"id": "c1", "scopes": ["dictate", "from_the_future"]}),
        )
        .await;
    assert_eq!(res.status, 400);
    assert_eq!(h.control.calls().len(), before, "nothing was asked of Anya");
}

#[tokio::test]
async fn removing_a_client_unpairs_it() {
    let control = FakeControl::sample();
    lock(&control.clients).extend([paired("c1", "Kitchen iPad"), paired("c2", "Laptop")]);
    let h = harness_with(control, ShellKind::Web).await;
    let res = h.post("/api/clients/remove", &json!({"id": "c1"})).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let left = res.json()["clients"].clone();
    assert_eq!(left.as_array().map(Vec::len), Some(1));
    assert_eq!(left[0]["id"], "c2");

    let res = h.post("/api/clients/remove", &json!({"id": "c1"})).await;
    assert_eq!(res.status, 404);
    assert_eq!(h.post("/api/clients/remove", &json!({})).await.status, 400);
    assert_eq!(h.control.calls(), ["remove_client c1"]);
}

#[tokio::test]
async fn the_registry_is_read_when_asked_and_its_failure_is_told() {
    let h = harness(ShellKind::Web).await;
    assert_eq!(h.get("/api/status").await.json()["plugins"], json!([]));
    let res = h.post("/api/plugins/market", &json!({})).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let p = &res.json()["plugins"][0];
    assert_eq!(p["name"], "Echo");
    assert_eq!(p["streaming"], true);
    assert_eq!(p["permissions"], json!(["network:example.com"]));

    *lock(&h.control.market) = Err(anya_runtime::PluginIssue::Fetch {
        url: "https://r.example/index.json".to_owned(),
        reason: "no route to host".to_owned(),
    });
    let res = h.post("/api/plugins/market", &json!({})).await;
    assert_eq!(res.status, 502);
    assert_eq!(
        res.json()["error"],
        "Cannot read the plugin registry: Cannot fetch https://r.example/index.json: no route to host"
    );
}

#[tokio::test]
async fn installing_a_plugin_leaves_it_off_until_it_is_enabled() {
    let h = harness(ShellKind::Web).await;
    let res = h.post("/api/plugins/install", &json!({"id": "nope"})).await;
    assert_eq!(res.status, 409);
    assert_eq!(
        res.json()["error"],
        "The plugin change failed: The registry has no plugin nope version latest"
    );
    assert!(lock(&h.control.plugins).is_empty());

    let res = h.post("/api/plugins/install", &json!({"id": "echo"})).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let p = res.json()["plugins"][0].clone();
    assert_eq!(
        (p["id"].as_str(), p["enabled"].clone()),
        (Some("echo"), json!(false))
    );
    assert_eq!(p["state"], "off");
    assert_eq!(p["name"], "Echo");
    assert_eq!(
        h.control.calls(),
        ["install_plugin nope", "install_plugin echo"]
    );

    let res = h
        .post("/api/plugins/enable", &json!({"id": "echo", "on": true}))
        .await;
    let p = res.json()["plugins"][0].clone();
    assert_eq!(
        (p["enabled"].clone(), p["state"].as_str()),
        (json!(true), Some("ok"))
    );
    assert_eq!(p["line"], "Loaded");
    assert_eq!(
        h.get("/api/state").await.json()["plugins"][0]["enabled"],
        true
    );
    let res = h
        .post("/api/plugins/enable", &json!({"id": "echo", "on": false}))
        .await;
    assert_eq!(res.json()["plugins"][0]["state"], "off");
}

#[tokio::test]
async fn plugin_and_pairing_refusals_read_in_the_users_language() {
    let control = FakeControl::sample();
    lock(&control.config).ui.language = "zh-CN".to_owned();
    let h = harness_with(control, ShellKind::Web).await;
    let res = h.post("/api/plugins/install", &json!({"id": "nope"})).await;
    assert_eq!(res.status, 409);
    assert_eq!(
        res.json()["error"],
        "插件操作失败：仓库里没有插件 nope latest"
    );
    *lock(&h.control.market) = Err(anya_runtime::PluginIssue::Integrity {
        what: "echo".to_owned(),
    });
    let res = h.post("/api/plugins/market", &json!({})).await;
    assert_eq!(res.status, 502);
    assert_eq!(
        res.json()["error"],
        "无法读取插件仓库：echo 的下载内容与完整性哈希不符，未安装任何内容"
    );
    let res = h.post("/api/pairing", &json!({})).await;
    assert_eq!(res.json()["error"], "无法配对：局域网共享已关闭");
    lock(&h.control.status).lan = Lan::Down {
        detail: "address in use".to_owned(),
    };
    let res = h.post("/api/pairing", &json!({})).await;
    assert_eq!(
        res.json()["error"],
        "无法配对：局域网监听未启动：address in use"
    );
}

#[tokio::test]
async fn a_failed_client_change_reads_in_the_users_language() {
    let control = FakeControl::sample();
    lock(&control.config).ui.language = "zh-CN".to_owned();
    lock(&control.clients).push(paired("c1", "Kitchen iPad"));
    *lock(&control.clients_write_error) = Some("disk full".to_owned());
    let h = harness_with(control, ShellKind::Web).await;
    let res = h.post("/api/clients/remove", &json!({"id": "c1"})).await;
    assert_eq!(res.status, 500);
    assert_eq!(
        res.json()["error"],
        "无法修改该应用：保存已配对的应用失败：disk full"
    );
    let res = h
        .post("/api/clients/scopes", &json!({"id": "c1", "scopes": []}))
        .await;
    assert_eq!(res.status, 500);
}

#[tokio::test]
async fn a_failed_plugin_says_why_in_the_users_language() {
    let control = FakeControl::sample();
    lock(&control.config).ui.language = "zh-CN".to_owned();
    lock(&control.plugins).push(anya_runtime::InstalledPlugin {
        id: "echo".to_owned(),
        version: "1.0.0".to_owned(),
        enabled: true,
        load: anya_runtime::PluginLoad::Failed {
            detail: "no entry".to_owned(),
        },
        title: None,
        description: None,
    });
    let h = harness_with(control, ShellKind::Web).await;
    let p = h.get("/api/status").await.json()["plugins"][0].clone();
    assert_eq!(p["state"], "missing");
    assert_eq!(p["line"], "未能加载：no entry");
    assert_eq!(p["name"], "echo");
}

#[tokio::test]
async fn plugins_that_are_not_installed_cannot_be_switched_or_removed() {
    let control = FakeControl::sample();
    let h = harness_with(control, ShellKind::Web).await;
    for (path, body) in [
        ("/api/plugins/enable", json!({"id": "echo", "on": true})),
        ("/api/plugins/remove", json!({"id": "echo"})),
    ] {
        let res = h.post(path, &body).await;
        assert_eq!(res.status, 404, "{path}");
        assert_eq!(res.json()["error"], "That plugin is not installed");
    }
    assert!(h.control.calls().is_empty(), "nothing was asked of Anya");

    h.post("/api/plugins/install", &json!({"id": "echo"})).await;
    let res = h.post("/api/plugins/remove", &json!({"id": "echo"})).await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert_eq!(res.json()["plugins"], json!([]));
    assert_eq!(h.post("/api/plugins/remove", &json!({})).await.status, 400);
}

#[tokio::test]
async fn the_lan_port_saves_and_zero_means_the_default() {
    let h = harness(ShellKind::Web).await;
    let mut c = h.get("/api/state").await.json()["config"].clone();
    c["subscription"]["lan"] = json!(true);
    c["subscription"]["lan_port"] = json!(9000);
    assert_eq!(h.post("/api/settings", &c).await.status, 200);
    c["subscription"]["lan_port"] = json!(0);
    assert_eq!(h.post("/api/settings", &c).await.status, 200);
    let saved = lock(&h.control.saved);
    assert_eq!(saved[0].subscription.lan_port, 9000);
    assert!(saved[1].subscription.lan);
    assert_eq!(
        saved[1].subscription.lan_port,
        anya_config::DEFAULT_LAN_PORT
    );
}

#[tokio::test]
async fn engine_ready_judges_unsaved_settings() {
    let h = harness(ShellKind::Web).await;
    let ask = |settings: Value| json!({"id": "cloud", "settings": settings});
    let yes = h
        .post("/api/engine/ready", &ask(json!({"api_key": "sk"})))
        .await;
    assert_eq!(yes.json()["ready"], true);
    let blank = h
        .post("/api/engine/ready", &ask(json!({"api_key": "   "})))
        .await;
    assert_eq!(blank.json()["ready"], false);
    let none = h.post("/api/engine/ready", &json!({"id": "whisper"})).await;
    assert_eq!(none.json()["ready"], true);
}

#[tokio::test]
async fn a_built_in_engine_switch_is_saved_in_the_config_and_shown_back() {
    let h = harness(ShellKind::Web).await;
    let before = h.get("/api/state").await.json();
    assert_eq!(before["engines"][0]["enabled"], true);
    assert!(
        before["engines"][2]["enabled"].is_null(),
        "an engine with no switch says so"
    );

    let mut c = edited(&h);
    c["plugins"]["builtin"] = json!({"cloud": false});
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let saved = lock(&h.control.saved).last().cloned().expect("a save");
    assert_eq!(saved.plugins.builtin.get("cloud"), Some(&false));
    assert_eq!(res.json()["state"]["engines"][0]["enabled"], false);
}

#[tokio::test]
async fn device_kinds_reach_the_page() {
    let control = FakeControl::sample();
    *lock(&control.devices) = Ok(vec![Device {
        id: "bh".to_owned(),
        name: "BlackHole 2ch".to_owned(),
        kind: DeviceKind::Loopback,
    }]);
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/state").await.json();
    assert_eq!(
        s["devices"],
        json!([{"id": "bh", "name": "BlackHole 2ch", "kind": "loopback"}])
    );
    assert!(s.get("devices_error").is_some_and(Value::is_null));
}

// ---- updates ----

fn available(via: UpdateVia) -> UpdateState {
    UpdateState::Available {
        version: "0.9.0".to_owned(),
        via,
    }
}

async fn shown_update(h: &Harness) -> Value {
    h.get("/api/status").await.json()["update"].clone()
}

#[tokio::test]
async fn each_update_state_shows_the_notice_and_actions_that_fit_it() {
    let h = harness(ShellKind::Web).await;
    let cases = [
        (UpdateState::Unknown, "Checking for updates…", false, false),
        (UpdateState::Current, "Anya is up to date", false, false),
        (
            available(UpdateVia::Homebrew),
            "Anya 0.9.0 is available",
            true,
            true,
        ),
        (
            available(UpdateVia::Other),
            "Anya 0.9.0 is available. Update it where you installed Anya from.",
            true,
            false,
        ),
        (
            UpdateState::Upgrading {
                version: "0.9.0".to_owned(),
            },
            "Upgrading to Anya 0.9.0…",
            true,
            false,
        ),
        (
            UpdateState::Restart {
                version: "0.9.0".to_owned(),
            },
            "Upgraded to Anya 0.9.0. Restart Anya to use it.",
            true,
            false,
        ),
        (
            UpdateState::Failed {
                detail: "no route to host".to_owned(),
            },
            "Could not check for updates: no route to host",
            false,
            false,
        ),
    ];
    for (state, line, notice, can_upgrade) in cases {
        *lock(&h.control.update) = state.clone();
        let u = shown_update(&h).await;
        assert_eq!(u["line"], line, "{state:?}");
        assert_eq!(u["notice"], notice, "{state:?}");
        assert_eq!(u["can_upgrade"], can_upgrade, "{state:?}");
    }
}

#[tokio::test]
async fn update_text_follows_the_ui_language_and_says_when_checks_are_off() {
    let h = harness(ShellKind::Web).await;
    *lock(&h.control.update) = available(UpdateVia::Homebrew);
    lock(&h.control.config).ui.language = "zh-CN".to_owned();
    assert_eq!(shown_update(&h).await["line"], "Anya 0.9.0 已发布");
    lock(&h.control.config).ui.language = "en".to_owned();
    *lock(&h.control.update) = UpdateState::Unknown;
    lock(&h.control.config).updates.check = false;
    assert_eq!(shown_update(&h).await["line"], "Update checks are off");
}

#[tokio::test]
async fn upgrading_says_to_restart_and_the_state_follows() {
    let h = harness(ShellKind::Web).await;
    *lock(&h.control.update) = available(UpdateVia::Homebrew);
    let res = h.post("/api/update/upgrade", &json!({})).await;
    assert_eq!(res.status, 200);
    let body = res.json();
    assert_eq!(
        body["message"],
        "Upgrade finished. Restart Anya to use the new version."
    );
    assert_eq!(
        body["update"]["line"],
        "Upgraded to Anya 0.9.0. Restart Anya to use it."
    );
    assert_eq!(body["update"]["can_upgrade"], false);
    assert_eq!(h.control.calls(), ["upgrade"]);
}

#[tokio::test]
async fn every_upgrade_issue_reads_in_the_users_language() {
    let h = harness(ShellKind::Web).await;
    let cases = [
        (
            UpdateIssue::NotHomebrew,
            "This Anya was not installed with Homebrew, so it cannot upgrade itself. Update it where you installed it from.",
            "这个 Anya 不是通过 Homebrew 安装的，无法自行升级，请到你安装它的来源处更新。",
        ),
        (
            UpdateIssue::BrewMissing,
            "Homebrew was not found on this computer.",
            "这台电脑上找不到 Homebrew。",
        ),
        (
            UpdateIssue::NothingToInstall,
            "There is no newer version to install.",
            "没有更新的版本可安装。",
        ),
        (
            UpdateIssue::Failed {
                detail: "Error: disk full".to_owned(),
            },
            "The upgrade failed: Error: disk full",
            "升级失败：Error: disk full",
        ),
    ];
    for (issue, english, chinese) in cases {
        *lock(&h.control.upgrade) = Err(issue.clone());
        for (language, text) in [("en", english), ("zh-CN", chinese)] {
            lock(&h.control.config).ui.language = language.to_owned();
            let res = h.post("/api/update/upgrade", &json!({})).await;
            assert_eq!(res.status, 409, "{issue:?}");
            assert_eq!(res.json()["error"], text, "{issue:?}");
        }
    }
}

#[tokio::test]
async fn check_now_asks_for_a_new_check() {
    let h = harness(ShellKind::Web).await;
    let res = h.post("/api/update/check", &json!({})).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["update"]["state"], "unknown");
    assert_eq!(h.control.calls(), ["check_update"]);
}

#[tokio::test]
async fn the_update_check_switch_saves() {
    let h = harness(ShellKind::Web).await;
    assert_eq!(
        h.get("/api/state").await.json()["config"]["updates"]["check"],
        true
    );
    let mut c = edited(&h);
    c["updates"]["check"] = json!(false);
    let res = h.post("/api/settings", &c).await;
    assert_eq!(res.status, 200);
    assert!(!lock(&h.control.saved).last().expect("a save").updates.check);
    assert_eq!(res.json()["state"]["config"]["updates"]["check"], false);
}

// ---- every word the page shows is in the catalog ----

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn catalog_keys(tag: &str) -> BTreeSet<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../i18n/locales")
        .join(tag);
    let mut out = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("locale dir") {
        let path = entry.expect("locale file").path();
        let map: BTreeMap<String, String> =
            serde_json::from_str(&read(&path)).expect("a JSON object of strings");
        out.extend(map.into_keys());
    }
    out
}

fn is_key(s: &str) -> bool {
    ["gui.", "common.", "error.", "overlay."]
        .iter()
        .any(|p| s.starts_with(p) && s.len() > p.len())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// The text of every `"…"` or `'…'` literal that starts right after `opener`.
fn literals_after<'a>(source: &'a str, opener: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    for (i, _) in source.match_indices(opener) {
        let rest = &source[i + opener.len()..];
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        if let Some(end) = rest[1..].find(quote) {
            out.push(&rest[1..1 + end]);
        }
    }
    out
}

/// Keys a source names: `gui.*` literals anywhere, any key as a `data-i18n*`
/// attribute or as the argument of `t(…)` / `tf(…)`.
fn referenced(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, quote) in source
        .char_indices()
        .filter(|(_, c)| *c == '"' || *c == '\'')
    {
        let rest = &source[i + 1..];
        if !rest.starts_with("gui.") {
            continue;
        }
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(rest.len());
        if rest[end..].starts_with(quote) && is_key(&rest[..end]) {
            out.insert(rest[..end].to_owned());
        }
    }
    for opener in [
        "t(",
        "tf(",
        "data-i18n=",
        "data-i18n-placeholder=",
        "data-i18n-title=",
        "data-i18n-aria=",
    ] {
        out.extend(
            literals_after(source, opener)
                .into_iter()
                .filter(|s| is_key(s))
                .map(str::to_owned),
        );
    }
    out
}

fn page_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = assets::FILES
        .iter()
        .filter(|(p, _, _)| *p != "/")
        .map(|(p, _, _)| root.join("assets").join(p.trim_start_matches('/')))
        .collect();
    files.push(root.join("src/web.rs"));
    for module in ["api", "assets", "auth", "reply", "view"] {
        files.push(root.join(format!("src/web/{module}.rs")));
    }
    files
        .into_iter()
        .map(|p| (p.display().to_string(), read(&p)))
        .collect()
}

#[test]
fn every_key_the_page_and_server_name_is_in_both_locales() {
    let en = catalog_keys("en");
    let zh = catalog_keys("zh-CN");
    let mut used = BTreeSet::new();
    for (file, text) in page_sources() {
        for key in referenced(&text) {
            assert!(en.contains(&key), "{file}: {key} is not in en");
            assert!(zh.contains(&key), "{file}: {key} is not in zh-CN");
            used.insert(key);
        }
    }
    let gui: BTreeSet<String> = en.into_iter().filter(|k| k.starts_with("gui.")).collect();
    let unused: Vec<_> = gui.difference(&used).collect();
    assert!(
        unused.is_empty(),
        "keys in gui.json nothing uses: {unused:?}"
    );
}

#[test]
fn the_html_carries_no_text_of_its_own() {
    for page in ["index.html", "panel.html"] {
        let html = read(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(page),
        );
        for chunk in html.split('>').skip(1) {
            let text = chunk.split('<').next().unwrap_or("").trim();
            assert!(
                text.is_empty() || text == "0",
                "{page} shows {text:?} without the catalog"
            );
        }
        for attr in ["placeholder=", "title=", "aria-label="] {
            assert!(
                !html.contains(&format!(" {attr}")),
                "{page} has a literal {attr}"
            );
        }
    }
}
