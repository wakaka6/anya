//! The command line. Help text comes from the catalog in the system's
//! language; the config's language is not known before `--config` is read.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anya_core::desktop::Desktop;
use anya_i18n::Catalog;
use clap::{Arg, ArgAction, ArgGroup, ArgMatches, Command as Clap, value_parser};

/// How the main command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The tray app and settings window; the TUI when no window can open.
    App,
    /// The terminal UI.
    Tui,
    /// The settings page in a browser on 127.0.0.1.
    Web,
    /// No UI; logs to stderr.
    Daemon,
}

/// The main command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// `--config`.
    pub config: Option<PathBuf>,
    /// `--backend`.
    pub backend: Option<Desktop>,
    /// `--debug`: debug hotkeys and details in the UIs.
    pub debug: bool,
    /// `--verbose`: debug-level logs.
    pub verbose: bool,
    /// The UI.
    pub mode: Mode,
    /// `--doctor`: check the environment and exit.
    pub doctor: bool,
    /// `--install`: copy this executable to `~/.local/bin` and exit.
    pub install: bool,
}

/// What to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run Anya.
    Run(Run),
    /// Print a running Anya's frames as NDJSON.
    Subscribe {
        /// `--config`, for its subscription address.
        config: Option<PathBuf>,
        /// `--addr`, overriding the config.
        addr: Option<String>,
        /// `--timeout`; `None` is unlimited.
        timeout: Option<Duration>,
        /// `--max-events`; `None` is unlimited.
        max_events: Option<u64>,
    },
    /// Serve a recorded frame stream to subscribers.
    Replay {
        /// The JSON-lines file.
        file: PathBuf,
        /// `--addr`; the default subscription address otherwise.
        addr: Option<String>,
        /// `--fast`: send frames as fast as possible instead of paced.
        fast: bool,
    },
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("{s:?}"))?;
    let secs = match unit {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        _ => return Err(format!("{s:?}")),
    };
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!("{s:?}"));
    }
    Ok(Duration::from_secs_f64(secs))
}

fn command(c: &Catalog) -> Clap {
    let config = || {
        Arg::new("config")
            .long("config")
            .value_name("PATH")
            .value_parser(value_parser!(PathBuf))
            .help(c.t("cli.help.config").to_owned())
    };
    let flag = |name: &'static str, key: &'static str| {
        Arg::new(name)
            .long(name)
            .action(ArgAction::SetTrue)
            .help(c.t(key).to_owned())
    };
    Clap::new("anya")
        .about(c.t("cli.about").to_owned())
        .version(env!("CARGO_PKG_VERSION"))
        .arg(config())
        .arg(
            Arg::new("backend")
                .long("backend")
                .value_name("darwin|wayland|x11")
                .value_parser(|s: &str| s.parse::<Desktop>().map_err(|e| e.to_string()))
                .help(c.t("cli.help.backend").to_owned()),
        )
        .arg(flag("debug", "cli.help.debug"))
        .arg(flag("verbose", "cli.help.verbose"))
        .arg(flag("tui", "cli.help.tui"))
        .arg(flag("web", "cli.help.web"))
        .arg(flag("daemon", "cli.help.daemon"))
        .arg(flag("doctor", "cli.help.doctor"))
        .arg(flag("install", "cli.help.install"))
        .group(
            ArgGroup::new("mode")
                .args(["tui", "web", "daemon"])
                .multiple(false),
        )
        .subcommand(
            Clap::new("subscribe")
                .about(c.t("cli.subscribe.about").to_owned())
                .arg(config())
                .arg(
                    Arg::new("addr")
                        .long("addr")
                        .value_name("ADDR")
                        .help(c.t("cli.help.addr").to_owned()),
                )
                .arg(
                    Arg::new("timeout")
                        .long("timeout")
                        .value_name("DURATION")
                        .value_parser(parse_duration)
                        .help(c.t("cli.help.timeout").to_owned()),
                )
                .arg(
                    Arg::new("max-events")
                        .long("max-events")
                        .value_name("N")
                        .value_parser(value_parser!(u64))
                        .help(c.t("cli.help.max_events").to_owned()),
                ),
        )
        .subcommand(
            Clap::new("replay")
                .about(c.t("cli.replay.about").to_owned())
                .arg(
                    Arg::new("file")
                        .required(true)
                        .value_name("FILE")
                        .value_parser(value_parser!(PathBuf))
                        .help(c.t("cli.help.replay_file").to_owned()),
                )
                .arg(
                    Arg::new("addr")
                        .long("addr")
                        .value_name("ADDR")
                        .help(c.t("cli.help.addr").to_owned()),
                )
                .arg(flag("fast", "cli.help.fast")),
        )
}

fn get_path(m: &ArgMatches, id: &str) -> Option<PathBuf> {
    m.get_one::<PathBuf>(id).cloned()
}

fn get_addr(m: &ArgMatches) -> Option<String> {
    m.get_one::<String>("addr")
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty())
}

/// Parses `args` (the program name first). Help, version and usage errors
/// come back as the error, ready to print with [`clap::Error::exit`].
pub fn parse(
    args: impl IntoIterator<Item = OsString>,
    c: &Catalog,
) -> Result<Command, clap::Error> {
    let m = command(c).try_get_matches_from(args)?;
    Ok(match m.subcommand() {
        Some(("subscribe", s)) => Command::Subscribe {
            config: get_path(s, "config"),
            addr: get_addr(s),
            timeout: s
                .get_one::<Duration>("timeout")
                .copied()
                .filter(|d| !d.is_zero()),
            max_events: s.get_one::<u64>("max-events").copied().filter(|n| *n > 0),
        },
        Some(("replay", s)) => Command::Replay {
            file: get_path(s, "file").unwrap_or_default(),
            addr: get_addr(s),
            fast: s.get_flag("fast"),
        },
        _ => Command::Run(Run {
            config: get_path(&m, "config"),
            backend: m.get_one::<Desktop>("backend").copied(),
            debug: m.get_flag("debug"),
            verbose: m.get_flag("verbose"),
            mode: if m.get_flag("daemon") {
                Mode::Daemon
            } else if m.get_flag("web") {
                Mode::Web
            } else if m.get_flag("tui") {
                Mode::Tui
            } else {
                Mode::App
            },
            doctor: m.get_flag("doctor"),
            install: m.get_flag("install"),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Command, clap::Error> {
        let c = Catalog::new("en");
        parse(
            std::iter::once("anya")
                .chain(args.iter().copied())
                .map(OsString::from),
            &c,
        )
    }

    #[test]
    fn no_flags_is_the_tray_app() {
        let Ok(Command::Run(r)) = run(&[]) else {
            panic!("not a run")
        };
        assert_eq!(r.mode, Mode::App);
        assert!(!r.doctor && !r.install && r.config.is_none() && r.backend.is_none());
        let Ok(Command::Run(r)) = run(&["--web", "--config", "/x.toml", "--backend", "X11"]) else {
            panic!("not a run")
        };
        assert_eq!((r.mode, r.backend), (Mode::Web, Some(Desktop::X11)));
        assert_eq!(r.config.as_deref(), Some(std::path::Path::new("/x.toml")));
    }

    #[test]
    fn two_ui_modes_are_refused_and_so_is_windows() {
        assert!(run(&["--tui", "--daemon"]).is_err());
        assert!(run(&["--backend", "windows"]).is_err());
    }

    #[test]
    fn subscribe_takes_bounds_and_zero_means_unlimited() {
        let got = run(&[
            "subscribe",
            "--timeout",
            "10m",
            "--max-events",
            "1",
            "--addr",
            " unix:~/a.sock ",
        ]);
        assert_eq!(
            got.ok(),
            Some(Command::Subscribe {
                config: None,
                addr: Some("unix:~/a.sock".into()),
                timeout: Some(Duration::from_secs(600)),
                max_events: Some(1),
            })
        );
        let Ok(Command::Subscribe {
            timeout,
            max_events,
            ..
        }) = run(&["subscribe", "--timeout", "0", "--max-events", "0"])
        else {
            panic!("not subscribe")
        };
        assert_eq!((timeout, max_events), (None, None));
        assert!(run(&["subscribe", "--timeout", "soon"]).is_err());
    }

    #[test]
    fn durations_read_like_people_write_them() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("1.5s"), Ok(Duration::from_millis(1500)));
        assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_duration("30"), Ok(Duration::from_secs(30)));
        assert!(parse_duration("-1s").is_err() && parse_duration("3d").is_err());
    }

    #[test]
    fn replay_needs_a_file() {
        assert!(run(&["replay"]).is_err());
        let got = run(&["replay", "f.jsonl", "--fast"]).ok();
        assert_eq!(
            got,
            Some(Command::Replay {
                file: "f.jsonl".into(),
                addr: None,
                fast: true
            })
        );
    }
}
