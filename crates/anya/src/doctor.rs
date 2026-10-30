//! The environment check: what Anya needs from the machine, printed in the
//! user's language. Adapters probe their own needs; this module adds the
//! engine check, the context a reader needs to act (which terminal to grant
//! permissions to, which config file to edit) and renders the report.
//!
//! Every start runs it; the report is printed only when something blocks
//! startup, or always with `--doctor`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anya_config::Recognition;
use anya_core::desktop::Desktop;
use anya_core::doctor::{Check, Severity};
use anya_core::engine::{Engine, EngineError};
use anya_i18n::Catalog;

/// One check per engine in use, each once: is it known, and are its settings
/// enough to open a session. Each says which uses rely on it. A warning:
/// Anya starts, and the settings window is where the credentials go.
pub fn engine_checks(
    recognition: &Recognition,
    engines: &[Arc<dyn Engine>],
    c: &Catalog,
) -> Vec<Check> {
    recognition
        .engines_in_use()
        .into_iter()
        .map(|id| {
            let uses = crate::builtin::use_names(recognition, id, c, "doctor.engine.use");
            let engine = engines.iter().find(|e| e.manifest().id == id);
            engine_check(
                id,
                &uses,
                engine.map(|e| e.as_ref()),
                &recognition.settings(id),
                c.locale(),
            )
        })
        .collect()
}

fn engine_check(
    id: &str,
    uses: &str,
    engine: Option<&dyn Engine>,
    settings: &BTreeMap<String, String>,
    locale: &str,
) -> Check {
    const CODE: &str = "engine";
    let Some(engine) = engine else {
        return Check::failed(CODE, Severity::Warning, "unknown")
            .with("engine", id)
            .with("uses", uses);
    };
    let title = engine.manifest().title.get(locale).to_owned();
    match engine.check(settings) {
        Ok(()) => Check::passed(CODE, Severity::Warning, "ready")
            .with("engine", title)
            .with("uses", uses),
        Err(e @ (EngineError::NotConfigured(_) | EngineError::Invalid(_))) => {
            let field = e.detail().to_owned();
            let label = engine
                .manifest()
                .fields
                .iter()
                .find(|f| f.key == field)
                .map_or(field.clone(), |f| f.label.get(locale).to_owned());
            let outcome = match e {
                EngineError::Invalid(_) => "invalid_value",
                _ => "not_configured",
            };
            Check::failed(CODE, Severity::Warning, outcome)
                .with("engine", title)
                .with("field", label)
                .with("uses", uses)
        }
        Err(e) => Check::failed(CODE, Severity::Warning, "invalid")
            .with("engine", title)
            .with("detail", e.to_string())
            .with("uses", uses),
    }
}

/// Who receives the permissions on macOS: the terminal that started Anya.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
    /// A terminal app by name.
    App(&'static str),
    /// A remote SSH session: the desktop terminal cannot be known.
    Ssh,
    /// Nothing recognised it.
    Unknown,
}

/// The terminal Anya runs in: its environment first, then SSH, then the
/// parent processes.
pub fn terminal() -> Terminal {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if let Some(app) = terminal_from_env(var) {
        return Terminal::App(app);
    }
    if var("SSH_CONNECTION").is_some() || var("SSH_CLIENT").is_some() {
        return Terminal::Ssh;
    }
    parent_commands()
        .find_map(|c| terminal_from_command(&c))
        .map_or(Terminal::Unknown, Terminal::App)
}

/// The terminal the environment names, if any.
pub fn terminal_from_env(var: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    if let Some(app) = ["LC_TERMINAL", "TERM_PROGRAM"]
        .iter()
        .find_map(|k| var(k).and_then(|v| terminal_from_name(&v)))
    {
        return Some(app);
    }
    let set = |keys: &[&str]| keys.iter().any(|k| var(k).is_some());
    [
        (&["WEZTERM_EXECUTABLE", "WEZTERM_PANE"][..], "WezTerm"),
        (&["KITTY_WINDOW_ID"][..], "kitty"),
        (&["ALACRITTY_WINDOW_ID"][..], "Alacritty"),
        (&["GHOSTTY_RESOURCES_DIR", "GHOSTTY_BIN_DIR"][..], "Ghostty"),
        (&["VSCODE_INJECTION"][..], "Visual Studio Code"),
    ]
    .into_iter()
    .find_map(|(keys, app)| set(keys).then_some(app))
}

fn terminal_from_name(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    [
        ("iterm", "iTerm2"),
        ("apple_terminal", "Terminal"),
        ("terminal", "Terminal"),
        ("ghostty", "Ghostty"),
        ("wezterm", "WezTerm"),
        ("vscode", "Visual Studio Code"),
        ("kitty", "kitty"),
        ("alacritty", "Alacritty"),
    ]
    .into_iter()
    .find_map(|(needle, app)| n.contains(needle).then_some(app))
}

/// The terminal an executable path belongs to, if any.
pub fn terminal_from_command(path: &str) -> Option<&'static str> {
    let p = path.to_lowercase();
    [
        (&["iterm.app", "iterm2"][..], "iTerm2"),
        (&["terminal.app/"][..], "Terminal"),
        (&["ghostty.app", "/ghostty"][..], "Ghostty"),
        (&["wezterm.app", "wezterm"][..], "WezTerm"),
        (
            &["visual studio code.app", "code helper"][..],
            "Visual Studio Code",
        ),
        (&["kitty.app", "/kitty"][..], "kitty"),
        (&["alacritty.app", "alacritty"][..], "Alacritty"),
    ]
    .into_iter()
    .find_map(|(needles, app)| needles.iter().any(|n| p.contains(n)).then_some(app))
}

/// The executable paths of Anya's ancestors, nearest first, read with `ps`.
fn parent_commands() -> impl Iterator<Item = String> {
    let mut pid = std::os::unix::process::parent_id();
    let mut seen = 0;
    std::iter::from_fn(move || {
        if pid <= 1 || seen >= 32 {
            return None;
        }
        seen += 1;
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "ppid=", "-o", "comm="])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let (ppid, comm) = line.split_once(char::is_whitespace)?;
        let ppid = ppid.trim().parse::<u32>().ok()?;
        pid = if ppid == pid { 1 } else { ppid };
        Some(comm.trim().to_owned())
    })
}

/// What the report knows besides the checks.
#[derive(Debug, Clone)]
pub struct Context {
    /// The desktop the checks were made for.
    pub desktop: Desktop,
    /// The config file in use (it may not exist yet).
    pub config: std::path::PathBuf,
    /// The terminal, on macOS.
    pub terminal: Option<Terminal>,
    /// The Linux desktop environment, e.g. `XDG_CURRENT_DESKTOP=KDE`.
    pub session: Vec<String>,
}

impl Context {
    /// The context of this process.
    pub fn detect(desktop: Desktop, config: &Path) -> Context {
        let session = [
            "XDG_CURRENT_DESKTOP",
            "DESKTOP_SESSION",
            "KDE_SESSION_VERSION",
        ]
        .iter()
        .filter_map(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| format!("{k}={v}"))
        })
        .collect();
        Context {
            desktop,
            config: config.to_owned(),
            terminal: (desktop == Desktop::Darwin).then(terminal),
            session,
        }
    }
}

/// The report, one line per entry, ending with the verdict.
pub fn render(c: &Catalog, ctx: &Context, checks: &[Check]) -> String {
    let platform = match ctx.desktop {
        Desktop::Darwin => "macOS",
        Desktop::Wayland | Desktop::X11 => "Linux",
    };
    let target = match &ctx.terminal {
        Some(Terminal::App(app)) => (*app).to_owned(),
        Some(Terminal::Ssh) => c.t("doctor.terminal.ssh_target").to_owned(),
        _ => c.t("doctor.terminal.unknown_target").to_owned(),
    };
    let config = ctx.config.display().to_string();
    let mut out = vec![
        c.t("doctor.title").to_owned(),
        c.tf(
            "doctor.platform",
            &[("platform", platform), ("backend", ctx.desktop.name())],
        ),
    ];
    match &ctx.terminal {
        Some(Terminal::App(app)) => out.push(c.tf("doctor.terminal", &[("terminal", app)])),
        Some(Terminal::Ssh) => out.push(c.tf(
            "doctor.terminal",
            &[("terminal", c.t("doctor.terminal.ssh"))],
        )),
        Some(Terminal::Unknown) => out.push(c.tf(
            "doctor.terminal",
            &[("terminal", c.t("doctor.terminal.unknown"))],
        )),
        None => {}
    }
    if !ctx.session.is_empty() {
        out.push(c.tf("doctor.session", &[("session", &ctx.session.join(" / "))]));
    }
    out.push(c.tf("doctor.config", &[("path", &config)]));
    out.push(String::new());

    for check in checks {
        let mut args: Vec<(&str, &str)> = vec![("terminal", &target), ("config", &config)];
        args.extend(check.values.iter().map(|(k, v)| (*k, v.as_str())));
        let key = |suffix: &str| format!("doctor.{}.{suffix}", check.code);
        // An outcome-specific key wins over the check's general one.
        let pick = |suffix: &str| {
            let specific = key(&format!("{}.{suffix}", check.outcome));
            let general = key(suffix);
            [specific, general]
                .into_iter()
                .find(|k| c.t(k) != k.as_str())
        };
        let mark = match (check.ok, check.severity) {
            (true, _) => "✓",
            (false, Severity::Warning) => "!",
            (false, Severity::Required) => "✗",
        };
        out.push(c.tf(
            "doctor.line",
            &[
                ("mark", mark),
                ("name", c.t(&key("name"))),
                ("detail", &c.tf(&key(check.outcome), &args)),
            ],
        ));
        if let Some(note) = pick("note") {
            out.extend(c.tf(&note, &args).lines().map(|l| format!("  {l}")));
        }
        if !check.ok
            && let Some(fix) = pick("fix")
        {
            out.push(format!(
                "  {}",
                c.tf("doctor.fix", &[("fix", &c.tf(&fix, &args))])
            ));
        }
    }
    out.push(String::new());
    out.push(
        c.t(if anya_core::doctor::healthy(checks) {
            "doctor.healthy"
        } else {
            "doctor.unhealthy"
        })
        .to_owned(),
    );
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use anya_core::doctor::Severity::{Required, Warning};

    fn ctx() -> Context {
        Context {
            desktop: Desktop::Darwin,
            config: "/home/u/.config/anya/config.toml".into(),
            terminal: Some(Terminal::App("Ghostty")),
            session: Vec::new(),
        }
    }

    #[test]
    fn a_failed_check_shows_its_fix_and_the_verdict_blocks_startup() {
        let checks = [
            Check::passed("engine", Warning, "ready").with("engine", "Doubao"),
            Check::failed("engine", Warning, "not_configured")
                .with("engine", "Doubao")
                .with("field", "API Key"),
        ];
        let en = render(&Catalog::new("en"), &ctx(), &checks);
        assert!(en.contains("Ghostty"), "{en}");
        assert!(en.contains("✓ ") && en.contains("! "), "{en}");
        assert!(
            en.contains("API Key") && en.contains("/home/u/.config/anya/config.toml"),
            "{en}"
        );
        assert!(
            en.trim_end()
                .ends_with(Catalog::new("en").t("doctor.healthy")),
            "a warning does not block: {en}"
        );
        let zh = render(
            &Catalog::new("zh-CN"),
            &ctx(),
            &[Check::failed("engine", Required, "unknown").with("engine", "nope")],
        );
        assert!(zh.contains("✗ ") && zh.contains("nope"), "{zh}");
        assert!(
            zh.trim_end()
                .ends_with(Catalog::new("zh-CN").t("doctor.unhealthy")),
            "{zh}"
        );
    }

    #[test]
    fn every_engine_in_use_is_checked_once_and_names_the_uses_that_rely_on_it() {
        let mut recognition = anya_config::Recognition::default();
        let shared = crate::builtin::ids()[0].clone();
        recognition.dictation.clone_from(&shared);
        recognition.continuous.clone_from(&shared);
        recognition.remote = "nope".into();
        let engines = crate::builtin::engines();
        let c = Catalog::new("en");

        let checks = engine_checks(&recognition, &engines, &c);
        assert_eq!(checks.len(), 2, "one per engine, not per use");
        assert!(checks.iter().all(|check| !check.blocks()));
        let out = render(&c, &ctx(), &checks);
        assert!(
            out.contains("dictation, continuous dictation"),
            "the shared engine lists both uses: {out}"
        );
        assert!(
            out.contains("no engine called \"nope\" (chosen for remote dictation)"),
            "{out}"
        );
        assert!(
            out.contains("[recognition] dictation, continuous or remote"),
            "{out}"
        );

        let zh_catalog = Catalog::new("zh-CN");
        let zh = render(
            &zh_catalog,
            &ctx(),
            &engine_checks(&recognition, &engines, &zh_catalog),
        );
        assert!(
            zh.contains("听写、持续转写") && zh.contains("远程听写"),
            "{zh}"
        );
    }

    #[test]
    fn a_passed_check_hides_its_fix() {
        let out = render(
            &Catalog::new("en"),
            &ctx(),
            &[Check::passed("engine", Warning, "ready").with("engine", "X")],
        );
        let fix = Catalog::new("en").tf("doctor.fix", &[("fix", "")]);
        assert!(!out.contains(fix.trim()), "{out}");
    }

    #[test]
    fn terminals_are_recognised_from_environment_and_process_paths() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            terminal_from_env(env(&[("TERM_PROGRAM", "iTerm.app")])),
            Some("iTerm2")
        );
        assert_eq!(
            terminal_from_env(env(&[("TERM_PROGRAM", "Apple_Terminal")])),
            Some("Terminal")
        );
        assert_eq!(
            terminal_from_env(env(&[("KITTY_WINDOW_ID", "1")])),
            Some("kitty")
        );
        assert_eq!(terminal_from_env(env(&[("TERM_PROGRAM", "tmux")])), None);
        assert_eq!(
            terminal_from_command("/Applications/Ghostty.app/Contents/MacOS/ghostty"),
            Some("Ghostty")
        );
        assert_eq!(terminal_from_command("-zsh"), None);
    }
}
