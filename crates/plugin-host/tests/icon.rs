//! Plugin icons as the settings see them: what a package may name, the
//! formats kept, and the store that names each icon after its content.

mod common;

use std::collections::HashMap;

use anya_plugin_host::{
    IconFormat, Icons, MAX_ICON_BYTES, integrity_of, package_icon, read_package,
};
use common::serve;

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"/>"#;
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// A package directory whose `anya.icon` is `icon`, with `files` in it.
fn package(icon: &str, files: &[(&str, &[u8])]) -> (tempfile::TempDir, anya_plugin_host::Package) {
    let dir = tempfile::tempdir().expect("tempdir");
    let json = serde_json::json!({
        "name": "notes",
        "version": "1.0.0",
        "anya": { "inline": "./inline.js", "icon": icon },
    });
    std::fs::write(dir.path().join("package.json"), json.to_string()).expect("write");
    std::fs::write(
        dir.path().join("inline.js"),
        "export const transform = (t) => t;",
    )
    .expect("write");
    for (name, bytes) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write");
    }
    let package = read_package(dir.path()).expect("package");
    (dir, package)
}

#[test]
fn a_package_icon_is_a_file_in_it_or_a_data_uri() {
    let (_dir, p) = package("./art/icon.svg", &[("art/icon.svg", SVG)]);
    assert_eq!(package_icon(&p).expect("file").as_deref(), Some(SVG));

    let (_dir, p) = package("data:image/svg+xml,%3Csvg%2F%3E", &[]);
    assert_eq!(
        package_icon(&p).expect("percent").as_deref(),
        Some(&b"<svg/>"[..])
    );

    let (_dir, p) = package("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==", &[]);
    assert_eq!(package_icon(&p).expect("base64").as_deref(), Some(PNG));
}

#[test]
fn a_package_icon_that_reaches_out_or_is_no_picture_is_refused() {
    let refused = |icon: &str, files: &[(&str, &[u8])]| {
        let (_dir, p) = package(icon, files);
        package_icon(&p).expect_err(icon).to_string()
    };
    assert!(refused("https://example.com/icon.png", &[]).contains("data:image"));
    assert!(refused("../icon.svg", &[]).contains("not a file in the package"));
    assert!(refused("/etc/hosts", &[]).contains("not a file in the package"));
    assert!(refused("./icon.txt", &[("icon.txt", b"hello")]).contains("not PNG"));
    let big = [PNG, &vec![0; MAX_ICON_BYTES]].concat();
    assert!(refused("./icon.png", &[("icon.png", &big)]).contains("1 MiB"));
}

#[test]
fn a_package_without_an_icon_has_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"plain","version":"1.0.0","anya":{"inline":"./inline.js"}}"#,
    )
    .expect("write");
    std::fs::write(
        dir.path().join("inline.js"),
        "export const transform = (t) => t;",
    )
    .expect("write");
    let p = read_package(dir.path()).expect("package");
    assert_eq!(package_icon(&p).expect("none"), None);
}

#[test]
fn formats_are_told_by_content_not_by_name() {
    let cases: [(&[u8], Option<IconFormat>); 7] = [
        (PNG, Some(IconFormat::Png)),
        (b"\xff\xd8\xff\xe0", Some(IconFormat::Jpeg)),
        (b"GIF89a....", Some(IconFormat::Gif)),
        (b"RIFF\0\0\0\0WEBPVP8 ", Some(IconFormat::Webp)),
        (b"\0\0\x01\0\x01\0", Some(IconFormat::Ico)),
        (b"<?xml version=\"1.0\"?>\n<SVG>", Some(IconFormat::Svg)),
        (b"<html><script>", None),
    ];
    for (bytes, format) in cases {
        assert_eq!(IconFormat::sniff(bytes), format, "{bytes:?}");
    }
}

#[test]
fn a_kept_icon_goes_by_the_name_its_integrity_gives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let icons = Icons::new(dir.path().join("icons"));
    let name = icons.keep(SVG).expect("keep");
    assert_eq!(Icons::name_of(&integrity_of(SVG)), Some(name.clone()));
    assert_eq!(icons.read(&name), Some((SVG.to_vec(), IconFormat::Svg)));
    assert_eq!(icons.keep(SVG).expect("again"), name);

    assert!(icons.keep(b"plain text").is_err());
    assert_eq!(icons.read("../icons"), None);
    assert_eq!(icons.read(&name.to_uppercase()), None);
    assert_eq!(Icons::name_of("sha1-AAAA"), None);
}

#[tokio::test]
async fn a_registry_icon_is_kept_only_when_its_hash_matches() {
    let base = serve(HashMap::from([("/icon.svg".to_owned(), SVG.to_vec())])).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let icons = Icons::new(dir.path().join("icons"));
    let url = format!("{base}/icon.svg");

    let wrong = integrity_of(PNG);
    assert!(icons.fetch(&url, &wrong).await.is_err());
    assert_eq!(icons.read(&Icons::name_of(&wrong).expect("name")), None);

    let right = integrity_of(SVG);
    let name = icons.fetch(&url, &right).await.expect("fetch");
    assert_eq!(Some(name.clone()), Icons::name_of(&right));
    assert_eq!(icons.read(&name), Some((SVG.to_vec(), IconFormat::Svg)));
}
