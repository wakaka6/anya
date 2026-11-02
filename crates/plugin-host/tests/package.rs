//! Reading a package's `package.json`.

mod common;

use anya_plugin_host::read_package;
use anya_protocol::Text;
use common::example;

#[test]
fn a_package_reads_as_its_anya_field_says() {
    let hooks = read_package(&example("hooks")).expect("hooks");
    assert_eq!(hooks.id, "hooks");
    assert_eq!(hooks.anya.host.as_deref(), Some("./hooks.js"));
    assert_eq!(hooks.anya.inline, None);
    assert_eq!(
        hooks
            .anya
            .fields
            .iter()
            .map(|f| f.key.as_str())
            .collect::<Vec<_>>(),
        ["suffix", "file"]
    );
    assert_eq!(hooks.anya.limits.transform_ms, Some(3000));
    assert_eq!(hooks.name, Text::Plain("hooks-example".to_owned()));

    let inline = read_package(&example("inline")).expect("inline");
    assert_eq!(inline.id, "inline");
    assert_eq!(inline.anya.inline.as_deref(), Some("./inline.js"));
    assert_eq!(inline.anya.host, None);
}

#[test]
fn a_directory_without_a_usable_package_json_is_an_error_naming_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = read_package(dir.path()).expect_err("no file");
    assert!(err.to_string().contains("package.json"), "{err}");

    std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("write");
    let err = read_package(dir.path()).expect_err("no anya field");
    assert!(err.to_string().contains("anya"), "{err}");

    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"x","anya":{"limits":{"bogus":1}}}"#,
    )
    .expect("write");
    assert!(
        read_package(dir.path()).is_err(),
        "a misspelt limit is refused"
    );
}

#[test]
fn a_package_that_could_not_run_is_refused_with_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("engine.js"), "export default {};").expect("write");
    let refused = |anya: &str| {
        std::fs::write(
            dir.path().join("package.json"),
            format!(r#"{{"name":"x","anya":{anya}}}"#),
        )
        .expect("write");
        read_package(dir.path()).expect_err(anya).to_string()
    };
    assert!(refused(r#"{"engine":"./engine.js"}"#).contains("anya.host"));
    assert!(refused(r#"{"protocol":1}"#).contains("neither"));
    assert!(refused(r#"{"host":"./missing.js"}"#).contains("missing.js"));
    assert!(refused(r#"{"host":"../engine.js"}"#).contains("../engine.js"));
    assert!(refused(r#"{"inline":"/etc/hosts"}"#).contains("/etc/hosts"));
    assert!(refused(r#"{"host":"./engine.js","protocol":2}"#).contains("protocol 2"));

    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"x","anya":{"host":"./engine.js","protocol":1}}"#,
    )
    .expect("write");
    assert!(read_package(dir.path()).is_ok());
}
