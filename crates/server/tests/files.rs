//! The files the server touches: the Unix socket and the discovery file.

mod common;

use std::os::unix::fs::PermissionsExt;

use anya_protocol::{Frame, PROTOCOL_VERSION};
use anya_server::{Address, Server, ServerError, parse_address};
use common::*;

fn mode(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[tokio::test]
async fn a_path_that_is_not_a_socket_is_never_touched() {
    let dir = TempDir::new("notsock");
    let file = dir.path().join("file");
    std::fs::write(&file, "keep me").expect("write");
    let sub = dir.path().join("dir");
    std::fs::create_dir(&sub).expect("mkdir");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&file, &link).expect("symlink");
    for path in [&file, &sub, &link] {
        let err = Server::start(hub(), Address::Unix(path.clone()), None, None)
            .await
            .expect_err("refused");
        assert!(
            matches!(&err, ServerError::NotASocket(p) if p == path),
            "{err}"
        );
    }
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "keep me");
    assert!(sub.is_dir());
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("link")
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn a_stale_socket_is_replaced() {
    let dir = TempDir::new("stale");
    let path = dir.path().join("a.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
    assert!(path.exists(), "the dead listener left its socket behind");
    let server = Server::start(hub(), Address::Unix(path.clone()), None, None)
        .await
        .expect("a stale socket is replaced");
    let mut client = connect(&server).await;
    assert!(matches!(next_frame(&mut client).await, Frame::Hello { .. }));
    server.stop().await;
}

#[tokio::test]
async fn a_live_socket_is_refused_and_keeps_serving() {
    let dir = TempDir::new("live");
    let path = dir.path().join("a.sock");
    let first = Server::start(hub(), Address::Unix(path.clone()), None, None)
        .await
        .expect("start");
    let err = Server::start(hub(), Address::Unix(path.clone()), None, None)
        .await
        .expect_err("a second instance is refused");
    assert!(matches!(err, ServerError::InUse(_)), "{err}");
    let mut client = connect(&first).await;
    assert!(matches!(next_frame(&mut client).await, Frame::Hello { .. }));
    first.stop().await;
}

#[tokio::test]
async fn a_missing_directory_is_an_error() {
    let dir = TempDir::new("nodir");
    let missing = dir.path().join("missing");
    let err = Server::start(hub(), Address::Unix(missing.join("a.sock")), None, None)
        .await
        .expect_err("refused");
    assert!(
        matches!(&err, ServerError::MissingDirectory(d) if *d == missing),
        "{err}"
    );
    assert!(!missing.exists(), "the directory is not created");
}

#[tokio::test]
async fn the_socket_is_owner_only_and_removed_on_stop() {
    let dir = TempDir::new("mode");
    let path = dir.path().join("a.sock");
    let server = Server::start(hub(), Address::Unix(path.clone()), None, None)
        .await
        .expect("start");
    assert_eq!(server.url(), format!("unix:{}", path.display()));
    assert_eq!(mode(&path), 0o600);
    server.stop().await;
    assert!(std::fs::symlink_metadata(&path).is_err(), "removed on stop");
}

#[tokio::test]
async fn the_discovery_file_says_where_anya_listens_until_it_stops() {
    let dir = TempDir::new("disc");
    let file = dir.path().join("run/anya/discovery.json");
    let server = Server::start(
        hub(),
        parse_address("127.0.0.1:0").expect("address"),
        Some(file.clone()),
        None,
    )
    .await
    .expect("start");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("JSON");
    assert_eq!(
        json,
        serde_json::json!({
            "url": server.url(),
            "version": "0.1.0-test",
            "protocol": PROTOCOL_VERSION,
            "pid": std::process::id(),
        })
    );
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(file.parent().expect("dir")), 0o700);
    server.stop().await;
    assert!(!file.exists(), "removed on stop");
}

#[tokio::test]
async fn a_discovery_file_that_cannot_be_written_stops_the_start() {
    let dir = TempDir::new("discfail");
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "").expect("write");
    let socket = dir.path().join("a.sock");
    let err = Server::start(
        hub(),
        Address::Unix(socket.clone()),
        Some(blocker.join("discovery.json")),
        None,
    )
    .await
    .expect_err("refused");
    assert!(matches!(err, ServerError::Discovery { .. }), "{err}");
    assert!(
        std::fs::symlink_metadata(&socket).is_err(),
        "the socket went with the server"
    );
}
