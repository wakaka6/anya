//! What a user of the registry and the plugin store sees: the index, the
//! integrity check, and what is on disk after each step.

mod common;

use std::collections::HashMap;

use anya_plugin_host::{
    DEFAULT_INDEX_URL, Installed, PluginDirs, PluginError, Registry, Store, parse_index,
    verify_integrity,
};
use common::{index, release, serve, tgz, tgz_raw};
use serde_json::{Value, json};

const PACKAGE_JSON: &[u8] = br#"{"name":"echo","version":"1.0.0","anya":{"host":"./engine.js"}}"#;

fn package() -> Vec<u8> {
    tgz(&[
        ("package.json", PACKAGE_JSON),
        ("engine.js", b"export default 1;"),
    ])
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(PluginDirs::under(dir.path()));
    (dir, store)
}

fn record(dir: &tempfile::TempDir) -> Value {
    let text = std::fs::read_to_string(dir.path().join("plugins/plugins.json")).expect("record");
    serde_json::from_str(&text).expect("json")
}

#[test]
fn the_default_index_is_the_dist_branch_of_anya_plugins() {
    assert_eq!(
        DEFAULT_INDEX_URL,
        "https://raw.githubusercontent.com/wakaka6/anya/registry/index.json"
    );
}

#[test]
fn an_index_of_another_format_is_refused() {
    let err = parse_index(br#"{"version": 2, "plugins": []}"#).expect_err("version");
    assert_eq!(
        err,
        PluginError::IndexVersion {
            found: 2,
            supported: 1
        }
    );
    assert!(matches!(
        parse_index(b"not json"),
        Err(PluginError::Index(_))
    ));
}

#[tokio::test]
async fn the_index_comes_over_http_and_from_a_file() {
    let body = package();
    let doc = index("echo", "1.0.0", "http://unused/echo.tgz", &body);
    let bytes = serde_json::to_vec(&doc).expect("json");
    let base = serve(HashMap::from([("/index.json".to_owned(), bytes.clone())])).await;

    let over_http = Registry::fetch(&format!("{base}/index.json"))
        .await
        .expect("http");
    assert_eq!(over_http.plugins[0].id, "echo");
    assert_eq!(over_http.plugins[0].versions[0].version, "1.0.0");

    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("index.json");
    std::fs::write(&file, &bytes).expect("write");
    let url = format!("file://{}", file.display());
    assert_eq!(Registry::fetch(&url).await.expect("file"), over_http);

    let missing = Registry::fetch(&format!("{base}/nope.json"))
        .await
        .expect_err("404");
    assert!(matches!(missing, PluginError::Fetch { .. }), "{missing}");
}

#[test]
fn integrity_is_checked_against_the_sha512_the_index_names() {
    let bytes = package();
    let good = anya_plugin_host::integrity_of(&bytes);
    assert!(good.starts_with("sha512-"));
    assert_eq!(verify_integrity("echo", &bytes, &good), Ok(()));

    let mut tampered = bytes.clone();
    tampered[20] ^= 1;
    assert!(matches!(
        verify_integrity("echo", &tampered, &good),
        Err(PluginError::Integrity { .. })
    ));
    assert!(matches!(
        verify_integrity("echo", &bytes, "sha1-AAAA"),
        Err(PluginError::IntegrityFormat(_))
    ));
}

#[tokio::test]
async fn install_enable_disable_remove_keep_plugins_json_in_step() {
    let (dir, store) = store();
    let body = package();
    let base = serve(HashMap::from([(
        "/echo-1.0.0.tgz".to_owned(),
        body.clone(),
    )]))
    .await;
    let doc = index("echo", "1.0.0", &format!("{base}/echo-1.0.0.tgz"), &body);
    let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");

    let installed = store
        .install(&index, "echo", "1.0.0", "0.1.0")
        .await
        .expect("install");
    assert_eq!(
        installed,
        Installed {
            id: "echo".to_owned(),
            version: "1.0.0".to_owned(),
            integrity: anya_plugin_host::integrity_of(&body),
            enabled: false,
            provides: vec![anya_protocol::Provide::Engine],
            host: true,
        }
    );
    assert_eq!(
        record(&dir),
        json!({"installed": [{
            "id": "echo", "version": "1.0.0",
            "integrity": anya_plugin_host::integrity_of(&body), "enabled": false,
            "provides": ["engine"], "host": true
        }]})
    );
    // `package/` is dropped: the package.json sits at the version's root.
    let package_dir = dir.path().join("plugins/echo/1.0.0");
    assert!(package_dir.join("package.json").is_file());
    assert!(package_dir.join("engine.js").is_file());
    assert_eq!(store.package_dir(&installed), package_dir);
    assert!(
        store.enabled().expect("read").is_empty(),
        "installing does not enable"
    );

    store.enable("echo").expect("enable");
    assert_eq!(record(&dir)["installed"][0]["enabled"], json!(true));
    assert_eq!(store.enabled().expect("read").len(), 1);
    store.disable("echo").expect("disable");
    assert_eq!(record(&dir)["installed"][0]["enabled"], json!(false));

    store.remove("echo").expect("remove");
    assert_eq!(record(&dir), json!({"installed": []}));
    assert!(!dir.path().join("plugins/echo").exists());
    assert_eq!(
        store.enable("echo"),
        Err(PluginError::NotInstalled("echo".to_owned()))
    );
    assert_eq!(
        store.remove("echo"),
        Err(PluginError::NotInstalled("echo".to_owned()))
    );
}

#[tokio::test]
async fn a_wrong_hash_installs_nothing() {
    let (dir, store) = store();
    let body = package();
    let base = serve(HashMap::from([("/echo.tgz".to_owned(), body.clone())])).await;
    let mut doc = index("echo", "1.0.0", &format!("{base}/echo.tgz"), &body);
    doc["plugins"][0]["versions"][0]["integrity"] = json!(anya_plugin_host::integrity_of(b"other"));
    let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");

    let err = store
        .install(&index, "echo", "1.0.0", "0.1.0")
        .await
        .expect_err("hash");
    assert!(matches!(err, PluginError::Integrity { .. }), "{err}");
    assert!(store.installed().expect("read").is_empty());
    assert!(!dir.path().join("plugins/echo").exists());
}

#[tokio::test]
async fn a_release_that_cannot_run_here_is_refused_before_download() {
    let (_dir, store) = store();
    let body = package();
    let url = "http://127.0.0.1:1/never-fetched.tgz";
    for (field, value, wanted) in [
        ("protocol", json!(2), "plugin protocol"),
        ("anya", json!(">=9.0.0"), "needs Anya"),
        ("provides", json!([]), "provides nothing"),
        ("provides", json!(["engine", "sink"]), "does not know"),
    ] {
        let mut doc = index("echo", "1.0.0", url, &body);
        doc["plugins"][0]["versions"][0][field] = value;
        let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");
        let err = store
            .install(&index, "echo", "1.0.0", "0.1.0")
            .await
            .expect_err(field);
        assert!(matches!(err, PluginError::Incompatible { .. }), "{err}");
        assert!(err.to_string().contains(wanted), "{err}");
    }
    let doc = index("echo", "1.0.0", url, &body);
    let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");
    assert!(matches!(
        store.install(&index, "echo", "2.0.0", "0.1.0").await,
        Err(PluginError::NotInIndex { .. })
    ));
    assert!(matches!(
        store.install(&index, "../evil", "1.0.0", "0.1.0").await,
        Err(PluginError::Invalid { .. })
    ));
}

#[tokio::test]
async fn a_tarball_cannot_write_outside_its_package() {
    let (dir, store) = store();
    let body = tgz_raw("package/../../escape.txt", b"x");
    let base = serve(HashMap::from([("/evil.tgz".to_owned(), body.clone())])).await;
    let doc = index("evil", "1.0.0", &format!("{base}/evil.tgz"), &body);
    let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");

    let err = store
        .install(&index, "evil", "1.0.0", "0.1.0")
        .await
        .expect_err("escape");
    assert!(matches!(err, PluginError::Archive { .. }), "{err}");
    assert!(!dir.path().join("plugins/escape.txt").exists());
    assert!(!dir.path().join("escape.txt").exists());
    assert!(store.installed().expect("read").is_empty());
}

#[tokio::test]
async fn updating_is_an_install_of_the_new_version_and_waits_for_enable() {
    let (dir, store) = store();
    let v1 = package();
    let v2 = tgz(&[
        (
            "package.json",
            br#"{"name":"echo","version":"1.1.0","anya":{"host":"./engine.js"}}"#,
        ),
        ("engine.js", b"export default 2;"),
    ]);
    let base = serve(HashMap::from([
        ("/1.tgz".to_owned(), v1.clone()),
        ("/2.tgz".to_owned(), v2.clone()),
    ]))
    .await;
    let doc = json!({"version": 1, "plugins": [{
        "id": "echo", "name": "Echo",
        "versions": [
            release("1.0.0", &format!("{base}/1.tgz"), &v1),
            release("1.1.0", &format!("{base}/2.tgz"), &v2)
        ]
    }]});
    let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");

    store
        .install(&index, "echo", "1.0.0", "0.1.0")
        .await
        .expect("v1");
    store.enable("echo").expect("enable");
    // Nothing installs a newer release by itself.
    assert_eq!(store.installed().expect("read")[0].version, "1.0.0");

    store
        .install(&index, "echo", "1.1.0", "0.1.0")
        .await
        .expect("v2");
    let now = store.installed().expect("read");
    assert_eq!(
        (now.len(), now[0].version.as_str(), now[0].enabled),
        (1, "1.1.0", false)
    );
    assert!(dir.path().join("plugins/echo/1.1.0/engine.js").is_file());
    assert!(!dir.path().join("plugins/echo/1.0.0").exists());
}

#[tokio::test]
async fn a_record_that_cannot_be_read_is_never_taken_for_an_empty_list() {
    let (dir, store) = store();
    let body = package();
    let base = serve(HashMap::from([("/echo.tgz".to_owned(), body.clone())])).await;
    let doc = index("echo", "1.0.0", &format!("{base}/echo.tgz"), &body);
    let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");
    store
        .install(&index, "echo", "1.0.0", "0.1.0")
        .await
        .expect("install");

    let path = dir.path().join("plugins/plugins.json");
    std::fs::write(&path, b"{\"installed\": [").expect("corrupt");

    assert!(matches!(store.installed(), Err(PluginError::Io { .. })));
    assert!(matches!(store.enabled(), Err(PluginError::Io { .. })));
    assert!(matches!(store.enable("echo"), Err(PluginError::Io { .. })));
    assert!(matches!(store.remove("echo"), Err(PluginError::Io { .. })));
    let err = store
        .install(&index, "echo", "1.0.0", "0.1.0")
        .await
        .expect_err("install");
    assert!(matches!(err, PluginError::Io { .. }), "{err}");
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"{\"installed\": [",
        "the broken file is left for the user"
    );
    assert!(dir.path().join("plugins/echo/1.0.0/engine.js").is_file());
}

#[tokio::test]
async fn a_change_keeps_the_record_it_replaces_as_a_backup() {
    let (dir, store) = store();
    let body = package();
    let base = serve(HashMap::from([("/echo.tgz".to_owned(), body.clone())])).await;
    let doc = index("echo", "1.0.0", &format!("{base}/echo.tgz"), &body);
    let index = parse_index(&serde_json::to_vec(&doc).expect("json")).expect("index");
    store
        .install(&index, "echo", "1.0.0", "0.1.0")
        .await
        .expect("install");
    let before = record(&dir);
    store.enable("echo").expect("enable");

    let backup =
        std::fs::read_to_string(dir.path().join("plugins/plugins.json.bak")).expect("backup");
    assert_eq!(
        serde_json::from_str::<Value>(&backup).expect("json"),
        before
    );
}

#[test]
fn another_process_holding_the_record_lock_delays_a_change() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (dir, store) = store();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("dir");
    std::fs::write(plugins.join("plugins.json"), b"{\"installed\": []}").expect("record");
    // What `anya plugin` in another process does: its own handle on the lock file.
    let other = std::fs::File::options()
        .create(true)
        .append(true)
        .open(plugins.join("plugins.json.lock"))
        .expect("lock file");
    other.lock().expect("lock");

    let (done, finished) = mpsc::channel();
    let waiting = std::thread::spawn(move || {
        let result = store.disable("echo");
        let _ = done.send(());
        result
    });
    assert!(
        finished.recv_timeout(Duration::from_millis(300)).is_err(),
        "the change went ahead while another process held the lock"
    );
    drop(other);
    finished
        .recv_timeout(Duration::from_secs(10))
        .expect("the change runs once the lock is free");
    assert_eq!(
        waiting.join().expect("thread"),
        Err(PluginError::NotInstalled("echo".to_owned()))
    );
}
