//! The page server as a browser and the page see it: raw HTTP against a
//! loopback listener, a scripted [`FakeControl`] behind it.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anya_core::audio::{Device, DeviceKind};
use anya_protocol::{ErrorCode, Source};
use anya_runtime::{Dictation, Entry, Notice, SaveError, Subscription};
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
    assert_eq!(res["text"]["gui.tabs.engine"], "Engine");
    assert_eq!(res["text"]["common.save"], "Save");
    lock(&h.control.config).ui.language = "zh-CN".to_owned();
    let res = h.get("/api/i18n").await.json();
    assert_eq!(res["locale"], "zh-CN");
    assert_eq!(res["text"]["gui.tabs.engine"], "识别引擎");
}

// ---- state and status ----

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

    let positions = s["positions"].as_array().expect("positions");
    assert_eq!(
        positions[0],
        json!({"value": "notch", "label": "Notch (macOS)"})
    );
    assert_eq!(
        positions.last(),
        Some(&json!({"value": "middle", "label": "middle"}))
    );
    let languages = s["languages"].as_array().expect("languages");
    assert_eq!(languages[0], json!({"value": "", "label": "Auto-detect"}));
    assert_eq!(
        languages[1],
        json!({"value": "zh-CN", "label": "简体中文 (zh-CN)"})
    );
    assert_eq!(
        languages.last(),
        Some(&json!({"value": "pt-BR", "label": "pt-BR"}))
    );
    assert_eq!(
        s["ui_languages"],
        json!([
            {"value": "system", "label": "Same as the system"},
            {"value": "en", "label": "English"},
            {"value": "zh-CN", "label": "简体中文"},
        ])
    );
}

#[tokio::test]
async fn state_in_chinese_and_with_an_engine_anya_lacks() {
    let control = FakeControl::sample();
    {
        let mut c = lock(&control.config);
        c.ui.language = "zh-CN".to_owned();
        c.recognition.engine = "  ".to_owned();
    }
    *lock(&control.devices) = Err("no audio server".to_owned());
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/state").await.json();
    assert_eq!(s["locale"], "zh-CN");
    assert_eq!(s["engines"][0]["title"], "云端流式");
    assert_eq!(s["engines"][0]["ready"], false);
    // A blank engine is the default one, shown although Anya lacks it.
    let current = s["config"]["recognition"]["engine"].clone();
    assert_eq!(current, anya_config::DEFAULT_ENGINE);
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
    assert_eq!(s["pill"], json!({"state": "failed", "label": "Error"}));
    assert_eq!(
        s["detail"],
        "Cannot reach the recognition service: connection refused  1 finishing in the background"
    );
    assert_eq!(s["subscription"]["state"], "ok");
    assert_eq!(
        s["subscription"]["line"],
        "Listening on ws://127.0.0.1:7590/asr · 2 subscribed"
    );
    assert_eq!(s["stats"]["sessions"], 42);
    assert_eq!(s["status"]["finishing"], 1);
    let activity = s["activity"].as_array().expect("activity");
    assert_eq!(activity[0]["text"], "Settings file changed; applied");
    assert_eq!(activity[1]["kind"], "final");
    assert_eq!(activity[1]["note"], "Pasted");
    assert_eq!(activity[2]["level"], "error");
    assert_eq!(
        activity[2]["text"],
        "Cannot reach the recognition service: connection refused"
    );
    assert_eq!(activity[3]["source"], "continuous");
    assert_eq!(activity[3]["note"], "Continuous transcription");
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
    }
    let s = h.get("/api/status").await.json();
    assert_eq!(s["pill"], json!({"state": "off", "label": "听写已关闭"}));
    assert_eq!(s["detail"], "");
    assert_eq!(
        s["subscription"],
        json!({"state": "missing", "line": "未启动：address in use"})
    );
    assert_eq!(s["activity"][1]["note"], "已上屏");
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
        })
        .collect();
    let h = harness_with(control, ShellKind::Web).await;
    let s = h.get("/api/status").await.json();
    let activity = s["activity"].as_array().expect("activity");
    assert_eq!(activity.len(), 100);
    assert_eq!(activity[0]["text"], "line 50");
    assert_eq!(activity[99]["note"], "Copied to the clipboard");
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
        "Saved, but the hotkeys could not be registered: Accessibility denied"
    );
}

#[tokio::test]
async fn invalid_settings_are_refused_per_field_in_both_languages() {
    let h = harness(ShellKind::Web).await;
    let mut c = edited(&h);
    c["dictation"]["hotkeys"] = json!("Ctrl+A");
    c["recognition"]["engine"] = json!("nope");
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
            {"fields": ["recognition.engine"], "message": "Unknown recognition engine: nope"},
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
