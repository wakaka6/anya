//! The layer rules hold on the real workspace.

use std::path::Path;

#[test]
fn layer_rules_hold() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let packages = xtask::arch::load(root).expect("read the workspace");
    let violations: Vec<String> = xtask::arch::check(&packages)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}
