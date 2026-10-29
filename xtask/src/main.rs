//! `cargo xtask check` runs the layer rules; `cargo xtask ci` runs the gate CI runs.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The CI gate, in order. The layer rules run inside `cargo test`.
const GATE: &[&[&str]] = &[
    &["fmt", "--all", "--check"],
    &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ],
    &["test", "--workspace"],
    &["deny", "check"],
];

fn main() -> ExitCode {
    let root = workspace_root();
    let ok = match std::env::args().nth(1).as_deref() {
        Some("check") => layers(&root),
        Some("ci") => gate(&root),
        _ => {
            eprintln!("usage: cargo xtask <check|ci>");
            eprintln!("  check  layer rules (ARCHITECTURE.md)");
            eprintln!("  ci     fmt, clippy, tests with the layer rules, cargo-deny");
            return ExitCode::from(2);
        }
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

fn layers(root: &Path) -> bool {
    let packages = match xtask::arch::load(root) {
        Ok(packages) => packages,
        Err(e) => {
            eprintln!("{e}");
            return false;
        }
    };
    let violations = xtask::arch::check(&packages);
    for v in &violations {
        eprintln!("{v}");
    }
    if violations.is_empty() {
        println!("layer rules hold for {} crates", packages.len());
    }
    violations.is_empty()
}

fn gate(root: &Path) -> bool {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    for args in GATE {
        println!("$ cargo {}", args.join(" "));
        match Command::new(&cargo).args(*args).current_dir(root).status() {
            Ok(status) if status.success() => {}
            Ok(_) => return false,
            Err(e) => {
                eprintln!("cargo {}: {e}", args[0]);
                return false;
            }
        }
    }
    true
}
