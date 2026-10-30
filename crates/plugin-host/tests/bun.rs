//! Downloading the runtime: only a zip with the pinned hash is unpacked.

use std::io::Write as _;

use anya_plugin_host::{BunPath, BunRelease, PINNED_BUN_VERSION, PluginError};
use ring::digest;

fn zip_with_bun(body: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("bun-test/bun", options).expect("start");
    zip.write_all(body).expect("write");
    zip.finish().expect("finish").into_inner()
}

fn sha256(bytes: &[u8]) -> String {
    digest::digest(&digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn release(dir: &tempfile::TempDir, zip: &[u8], sha256: String) -> BunRelease {
    let file = dir.path().join("bun.zip");
    std::fs::write(&file, zip).expect("write");
    BunRelease {
        version: "9.9.9".to_owned(),
        url: format!("file://{}", file.display()),
        sha256,
    }
}

#[tokio::test]
async fn a_zip_with_the_pinned_hash_unpacks_to_an_executable() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let zip = zip_with_bun(b"#!/bin/sh\necho 1.3.0\n");
    let runtime = dir.path().join("runtime");
    let bun = BunPath::download(&runtime, &release(&dir, &zip, sha256(&zip)))
        .await
        .expect("download");
    assert_eq!(bun.0, runtime.join("bun/9.9.9/bun"));
    assert_eq!(
        std::fs::read(&bun.0).expect("read"),
        b"#!/bin/sh\necho 1.3.0\n"
    );
    assert_eq!(
        std::fs::metadata(&bun.0)
            .expect("meta")
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
}

#[tokio::test]
async fn a_zip_with_another_hash_is_not_unpacked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let zip = zip_with_bun(b"evil");
    let runtime = dir.path().join("runtime");
    let err = BunPath::download(&runtime, &release(&dir, &zip, sha256(b"other")))
        .await
        .expect_err("hash");
    assert!(matches!(err, PluginError::Integrity { .. }), "{err}");
    assert!(!runtime.exists(), "nothing is written for a bad download");
}

#[test]
fn the_pinned_release_names_this_platforms_zip() {
    let release = BunRelease::pinned().expect("macOS and Linux on x64 and arm64 are pinned");
    assert_eq!(release.version, PINNED_BUN_VERSION);
    assert!(
        release
            .url
            .starts_with("https://github.com/oven-sh/bun/releases/download/")
    );
    assert!(release.url.ends_with(".zip"));
    assert_eq!(release.sha256.len(), 64);
    assert!(release.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
}
