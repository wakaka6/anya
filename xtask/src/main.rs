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

/// Where the plugins this repository releases live; `bun test` runs their tests.
const PLUGINS: &str = "plugins";

fn main() -> ExitCode {
    let root = workspace_root();
    let ok = match std::env::args().nth(1).as_deref() {
        Some("check") => layers(&root),
        Some("ci") => gate(&root),
        _ => {
            eprintln!("usage: cargo xtask <check|ci>");
            eprintln!("  check  layer rules (ARCHITECTURE.md)");
            eprintln!("  ci     fmt, clippy, tests with the layer rules, cargo-deny, plugin tests");
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
    println!("$ bun test ({PLUGINS}/)");
    match Command::new("bun")
        .arg("test")
        .current_dir(root.join(PLUGINS))
        .status()
    {
        Ok(status) => status.success(),
        Err(e) => {
            eprintln!("bun test: {e}; the plugins' tests need Bun 1.2 or newer on PATH");
            false
        }
    }
}
