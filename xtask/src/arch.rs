//! The layer rules. ARCHITECTURE.md says why each holds; this module is the one
//! place that says what they are. `tests/layers.rs` runs them on the real
//! workspace, so `cargo test` fails on a crate that breaks one.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// Where a crate sits. Its layer decides what it may depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// `anya-protocol`: wire formats.
    Protocol,
    /// `anya-core`: the seams and the session controller.
    /// `anya-plugin-rt` sits beside it: the plugin runtime, which names no seam.
    Core,
    /// When sessions open and end: dictation, continuous transcription.
    /// Pure logic over core's seams; the app feeds them input events.
    Trigger,
    /// Platform adapters: audio, hotkey, output, overlay.
    Adapter,
    /// One speech engine per crate, `anya-engine-<id>`.
    Engine,
    /// What serves other programs: the subscriber server, the plugin host.
    Service,
    /// The binary and the desktop app, where everything is wired together.
    App,
    /// Workspace tooling.
    Tooling,
}

impl Layer {
    /// The layer of a workspace crate, or `None` when no rule covers it.
    pub fn of(package: &str) -> Option<Layer> {
        Some(match package {
            "anya-protocol" => Layer::Protocol,
            "anya-core" | "anya-plugin-rt" => Layer::Core,
            "anya-dictation" | "anya-continuous" => Layer::Trigger,
            "anya-audio" | "anya-hotkey" | "anya-output" | "anya-overlay" | "anya-focus" => {
                Layer::Adapter
            }
            "anya-server" | "anya-plugin-host" | "anya-script" | "anya-update" | "anya-whisper" => {
                Layer::Service
            }
            "anya" | "anya-config" | "anya-i18n" | "anya-runtime" | "anya-tui" | "anya-gui" => {
                Layer::App
            }
            "xtask" => Layer::Tooling,
            p if p.starts_with(ENGINE_PREFIX) => Layer::Engine,
            _ => return None,
        })
    }

    /// Downward only, and never sideways: two adapters meet in core, wired by the app.
    fn may_use(self, dep: Layer) -> bool {
        match self {
            Layer::Protocol | Layer::Tooling => false,
            Layer::Core => dep == Layer::Protocol,
            Layer::Trigger | Layer::Adapter | Layer::Engine | Layer::Service => {
                matches!(dep, Layer::Protocol | Layer::Core)
            }
            Layer::App => dep != Layer::Tooling,
        }
    }

    /// Platform bindings and `unsafe` are allowed only here.
    fn touches_platform(self) -> bool {
        matches!(self, Layer::Adapter | Layer::App)
    }

    /// The external crates this layer may declare; `None` is any.
    fn external_allowlist(self) -> Option<&'static [&'static str]> {
        match self {
            Layer::Protocol => Some(&["serde"]),
            Layer::Core | Layer::Trigger => Some(&["serde", "thiserror", "tokio", "tracing"]),
            _ => None,
        }
    }
}

const ENGINE_PREFIX: &str = "anya-engine-";

/// Crates that bind an operating system or a GUI toolkit.
const PLATFORM_CRATES: &[&str] = &[
    "cpal",
    "evdev",
    "x11rb",
    "x11",
    "wayland-client",
    "smithay-client-toolkit",
    "objc2",
    "objc2-foundation",
    "objc2-app-kit",
    "core-foundation",
    "core-graphics",
    "screencapturekit",
    "libc",
    "nix",
    "tauri",
    "tao",
    "wry",
    "tray-icon",
    "muda",
    "gtk",
    "glib",
    "webkit2gtk",
    "alsa",
    "coreaudio-rs",
    "coreaudio-sys",
    "block2",
    "objc2-core-foundation",
    "objc2-core-audio",
    "objc2-core-graphics",
    "objc2-application-services",
    "wayland-protocols",
    "wayland-protocols-wlr",
    "xkbcommon",
    "input-linux",
    "uinput",
    "arboard",
    "enigo",
    "rdev",
];

/// Anya runs on Linux and macOS only (docs/decisions/0001).
const WINDOWS_CRATES: &[&str] = &["windows", "windows-sys", "winapi"];

/// A workspace crate as the rules see it.
#[derive(Debug, Clone)]
pub struct Package {
    /// Package name.
    pub name: String,
    /// Declared dependencies.
    pub dependencies: Vec<Dependency>,
    /// Every `.rs` file under `src/`.
    pub sources: Vec<SourceFile>,
}

/// A declared dependency.
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Package name.
    pub name: String,
    /// Which section declares it.
    pub kind: DepKind,
}

/// The section a dependency is declared in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    /// `[dependencies]`
    Normal,
    /// `[build-dependencies]`
    Build,
    /// `[dev-dependencies]`: free, tests only.
    Dev,
}

/// A source file and its text.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Where it is.
    pub path: PathBuf,
    /// What it says.
    pub text: String,
}

/// A broken rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The crate that breaks it.
    pub package: String,
    /// What is wrong and where to read why.
    pub rule: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.package, self.rule)
    }
}

/// Checks every rule on `packages`, the whole workspace.
pub fn check(packages: &[Package]) -> Vec<Violation> {
    let members: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let engines: Vec<&str> = members
        .iter()
        .filter_map(|n| n.strip_prefix(ENGINE_PREFIX))
        .collect();
    let mut out = Vec::new();
    for p in packages {
        let mut broke = |rule: String| {
            out.push(Violation {
                package: p.name.clone(),
                rule,
            })
        };
        let Some(layer) = Layer::of(&p.name) else {
            broke(
                "has no layer: add it to Layer::of in xtask/src/arch.rs and to ARCHITECTURE.md"
                    .to_owned(),
            );
            continue;
        };
        for d in p.dependencies.iter().filter(|d| d.kind != DepKind::Dev) {
            let dep = d.name.as_str();
            if members.contains(&dep) {
                if !Layer::of(dep).is_some_and(|l| layer.may_use(l)) {
                    broke(format!(
                        "{layer:?} may not depend on {dep} (ARCHITECTURE.md, Layers)"
                    ));
                }
            } else if WINDOWS_CRATES.contains(&dep) {
                broke(format!(
                    "{dep} is for Windows, which Anya does not support (docs/decisions/0001)"
                ));
            } else if PLATFORM_CRATES.contains(&dep) && !layer.touches_platform() {
                broke(format!(
                    "{dep} binds the platform; only adapters and the app may use it"
                ));
            } else if let Some(allowed) = layer.external_allowlist()
                && !allowed.contains(&dep)
            {
                broke(format!(
                    "{layer:?} keeps to {allowed:?}; {dep} belongs in an adapter or a service"
                ));
            }
        }
        if layer == Layer::Tooling {
            continue;
        }
        for f in &p.sources {
            let path = f.path.display();
            if !layer.touches_platform()
                && (f.text.contains("allow(unsafe_code)") || f.text.contains("expect(unsafe_code)"))
            {
                broke(format!("{path}: unsafe is for adapters and the app"));
            }
            if !matches!(layer, Layer::Engine | Layer::App) {
                for engine in engines.iter().filter(|e| mentions(&f.text, e)) {
                    broke(format!(
                        "{path} names the engine {engine:?}; read engines through their Manifest"
                    ));
                }
            }
        }
    }
    out
}

/// `word` appears in `text` as a whole word, ignoring ASCII case. `word` is lowercase.
fn mentions(text: &str, word: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let in_word = |c: char| c.is_ascii_alphanumeric();
    text.match_indices(word).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(in_word)
            && !text[i + word.len()..].chars().next().is_some_and(in_word)
    })
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetaPackage>,
}

#[derive(Deserialize)]
struct MetaPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetaDependency>,
}

#[derive(Deserialize)]
struct MetaDependency {
    name: String,
    kind: Option<String>,
}

/// Reads the workspace at `root` through `cargo metadata`.
pub fn load(root: &Path) -> Result<Vec<Package>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: Metadata =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))?;
    meta.packages
        .into_iter()
        .map(|p| {
            let mut sources = Vec::new();
            if let Some(dir) = p.manifest_path.parent() {
                read_sources(&dir.join("src"), &mut sources)?;
            }
            let dependencies = p
                .dependencies
                .into_iter()
                .map(|d| Dependency {
                    kind: match d.kind.as_deref() {
                        Some("dev") => DepKind::Dev,
                        Some("build") => DepKind::Build,
                        _ => DepKind::Normal,
                    },
                    name: d.name,
                })
                .collect();
            Ok(Package {
                name: p.name,
                dependencies,
                sources,
            })
        })
        .collect()
}

fn read_sources(dir: &Path, out: &mut Vec<SourceFile>) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_dir() {
            read_sources(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.push(SourceFile { path, text });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_owned(),
            dependencies: deps
                .iter()
                .map(|d| Dependency {
                    name: (*d).to_owned(),
                    kind: DepKind::Normal,
                })
                .collect(),
            sources: Vec::new(),
        }
    }

    fn with_source(mut p: Package, text: &str) -> Package {
        p.sources.push(SourceFile {
            path: PathBuf::from("src/lib.rs"),
            text: text.to_owned(),
        });
        p
    }

    fn broken(packages: &[Package]) -> Vec<String> {
        check(packages).into_iter().map(|v| v.package).collect()
    }

    fn base() -> Vec<Package> {
        vec![
            pkg("anya-protocol", &["serde"]),
            pkg("anya-core", &["anya-protocol", "tokio"]),
        ]
    }

    #[test]
    fn the_layers_as_designed_pass() {
        let mut ws = base();
        ws.push(pkg("anya-output", &["anya-core", "anya-protocol", "x11rb"]));
        ws.push(pkg("anya-engine-example", &["anya-core", "reqwest"]));
        ws.push(pkg(
            "anya",
            &["anya-output", "anya-engine-example", "anya-core"],
        ));
        assert_eq!(check(&ws), []);
    }

    #[test]
    fn siblings_meet_only_through_core() {
        let mut ws = base();
        ws.push(pkg("anya-output", &["anya-core"]));
        ws.push(pkg("anya-hotkey", &["anya-core", "anya-output"]));
        assert_eq!(broken(&ws), ["anya-hotkey"]);
    }

    #[test]
    fn nothing_depends_upward() {
        let mut ws = base();
        ws.push(pkg("anya-server", &["anya-core"]));
        ws[1].dependencies.push(Dependency {
            name: "anya-server".to_owned(),
            kind: DepKind::Normal,
        });
        assert_eq!(broken(&ws), ["anya-core"]);
    }

    #[test]
    fn core_keeps_to_its_runtime_crates_outside_tests() {
        let mut ws = base();
        ws[1].dependencies.push(Dependency {
            name: "reqwest".to_owned(),
            kind: DepKind::Normal,
        });
        assert_eq!(broken(&ws), ["anya-core"]);
        ws[1].dependencies.last_mut().expect("added").kind = DepKind::Dev;
        assert_eq!(check(&ws), []);
    }

    #[test]
    fn platform_bindings_stay_in_adapters_and_the_app() {
        let mut ws = base();
        ws.push(pkg("anya-server", &["anya-core", "cpal"]));
        ws.push(pkg("anya-audio", &["anya-core", "cpal"]));
        assert_eq!(broken(&ws), ["anya-server"]);
    }

    #[test]
    fn windows_crates_are_refused_everywhere() {
        let mut ws = base();
        ws.push(pkg("anya-audio", &["anya-core", "windows-sys"]));
        assert_eq!(broken(&ws), ["anya-audio"]);
    }

    #[test]
    fn triggers_stay_pure_and_meet_adapters_through_the_app() {
        let mut ws = base();
        ws.push(pkg("anya-hotkey", &["anya-core"]));
        ws.push(pkg("anya-dictation", &["anya-core", "tokio"]));
        assert_eq!(check(&ws), []);
        ws.push(pkg("anya-continuous", &["anya-core", "anya-hotkey"]));
        ws.push(pkg("anya-server", &["anya-dictation"]));
        ws[3].dependencies.push(Dependency {
            name: "reqwest".to_owned(),
            kind: DepKind::Normal,
        });
        assert_eq!(
            broken(&ws),
            ["anya-dictation", "anya-continuous", "anya-server"]
        );
    }

    #[test]
    fn a_crate_without_a_layer_is_refused() {
        let mut ws = base();
        ws.push(pkg("anya-cache", &[]));
        assert_eq!(broken(&ws), ["anya-cache"]);
    }

    #[test]
    fn an_engine_is_named_only_in_its_crate_and_the_app() {
        let mut ws = base();
        ws.push(with_source(
            pkg("anya-engine-doubao", &["anya-core"]),
            "// Doubao's quirks",
        ));
        ws.push(with_source(
            pkg("anya", &["anya-engine-doubao"]),
            "anya_engine_doubao::register()",
        ));
        assert_eq!(check(&ws), []);
        ws[1] = with_source(ws[1].clone(), "// a doubaox name is another word");
        assert_eq!(check(&ws), []);
        ws[1] = with_source(ws[1].clone(), "if id == \"Doubao\" {}");
        assert_eq!(broken(&ws), ["anya-core"]);
    }

    #[test]
    fn unsafe_is_for_adapters_and_the_app() {
        let mut ws = base();
        ws.push(with_source(
            pkg("anya-audio", &["anya-core"]),
            "#![allow(unsafe_code)]",
        ));
        assert_eq!(check(&ws), []);
        ws[1] = with_source(ws[1].clone(), "#[expect(unsafe_code)]");
        assert_eq!(broken(&ws), ["anya-core"]);
    }
}
