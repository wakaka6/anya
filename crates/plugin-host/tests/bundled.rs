//! A plugin built into Anya, as its user sees it: listed like a registry
//! entry, installed from the binary with the checks of a download.

use anya_plugin_host::{Bundled, PluginDirs, PluginError, Store, package_icon, read_package};
use anya_protocol::Provide;

const NOTES_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"/>"#;

const NOTES: Bundled = Bundled {
    provides: &[Provide::Action],
    files: &[
        (
            "package.json",
            br#"{"name":"notes","version":"1.2.0","private":true,"anya":{"name":"Notes","icon":"./icon.svg","host":"./host.js","compat":">=0.1.0","protocol":1,"permissions":["fs:write:notes"]}}"#,
        ),
        ("host.js", b"export const action = () => {};"),
        ("icon.svg", NOTES_SVG),
    ],
};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(PluginDirs::under(dir.path()));
    (dir, store)
}

// Anya 1.3 ships notes 1.3.0 where 1.2 shipped 1.2.0.
const NOTES_NEXT: Bundled = Bundled {
    provides: &[Provide::Action],
    files: &[
        (
            "package.json",
            br#"{"name":"notes","version":"1.3.0","private":true,"anya":{"name":"Notes","host":"./host.js","compat":">=0.1.0","protocol":1}}"#,
        ),
        ("host.js", b"export const action = () => 1;"),
    ],
};

#[test]
fn it_follows_the_binary_and_stays_as_the_user_left_it() {
    let (dir, store) = store();
    assert_eq!(
        store.update_bundled(&NOTES, "0.1.0").expect("none"),
        None,
        "not installed"
    );
    assert!(
        store.installed().expect("record").is_empty(),
        "an update installs nothing new"
    );

    store.install_bundled(&NOTES, "0.1.0").expect("install");
    assert_eq!(store.update_bundled(&NOTES, "0.1.0").expect("same"), None);
    store.enable("notes").expect("enable");

    let (from, now) = store
        .update_bundled(&NOTES_NEXT, "0.1.0")
        .expect("update")
        .expect("updated");
    assert_eq!(from, "1.2.0");
    assert_eq!(
        store.installed().expect("record"),
        std::slice::from_ref(&now)
    );
    assert_eq!((now.version.as_str(), now.enabled), ("1.3.0", true));
    assert_eq!(
        std::fs::read_to_string(store.package_dir(&now).join("host.js")).expect("host.js"),
        "export const action = () => 1;"
    );
    assert!(
        !dir.path().join("plugins/notes/1.2.0").exists(),
        "the old files go"
    );

    store.disable("notes").expect("disable");
    assert_eq!(
        store
            .update_bundled(&NOTES, "0.1.0")
            .expect("back")
            .map(|(from, _)| from)
            .as_deref(),
        Some("1.3.0"),
        "it is whatever this Anya ships"
    );
    assert!(
        !store.installed().expect("record")[0].enabled,
        "off stays off"
    );
}

#[test]
fn it_reads_as_a_registry_entry_with_one_release() {
    let entry = NOTES.entry().expect("entry");
    assert_eq!(entry.id, "notes");
    assert_eq!(entry.name.get("en"), "Notes");
    let [release] = entry.versions.as_slice() else {
        panic!("one release: {:?}", entry.versions);
    };
    assert_eq!(release.version, "1.2.0");
    assert_eq!(release.provides, [Provide::Action]);
    assert!(release.host);
    assert_eq!(release.permissions, ["fs:write:notes"]);
    assert_eq!(
        (
            release.icon.as_deref(),
            NOTES.icon().expect("icon").as_deref()
        ),
        (None, Some(NOTES_SVG)),
        "its icon comes from the binary, never from a URL"
    );
    assert_eq!(
        NOTES.entry().expect("again").versions[0].integrity,
        release.integrity,
        "the same files hash the same"
    );
}

#[test]
fn it_installs_without_the_network_and_waits_for_enable() {
    let (_dir, store) = store();
    let installed = store.install_bundled(&NOTES, "0.1.0").expect("install");
    assert_eq!(
        (installed.id.as_str(), installed.version.as_str()),
        ("notes", "1.2.0")
    );
    assert!(!installed.enabled);
    assert!(installed.host);
    assert_eq!(
        installed.integrity,
        NOTES.entry().expect("entry").versions[0].integrity
    );
    assert_eq!(
        store.installed().expect("record"),
        std::slice::from_ref(&installed)
    );
    let package = read_package(&store.package_dir(&installed)).expect("unpacked");
    assert_eq!(package.anya.host.as_deref(), Some("./host.js"));
    assert_eq!(
        std::fs::read_to_string(package.dir.join("host.js")).expect("host.js"),
        "export const action = () => {};"
    );
    assert_eq!(
        package_icon(&package).expect("icon").as_deref(),
        Some(NOTES_SVG)
    );
}

#[test]
fn it_is_refused_where_it_cannot_run_and_when_its_package_is_broken() {
    let (_dir, store) = store();
    let err = store.install_bundled(&NOTES, "0.0.9").expect_err("too old");
    assert!(matches!(err, PluginError::Incompatible { .. }), "{err}");

    let missing = Bundled {
        provides: &[Provide::Action],
        files: &[(
            "package.json",
            br#"{"name":"gone","version":"1.0.0","anya":{"host":"./host.js","compat":"*"}}"#,
        )],
    };
    let err = store
        .install_bundled(&missing, "0.1.0")
        .expect_err("no entry");
    assert!(
        err.to_string().contains("not a file in the package"),
        "{err}"
    );
    assert!(store.installed().expect("record").is_empty());
}
