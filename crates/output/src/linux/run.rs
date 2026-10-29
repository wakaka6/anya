//! Running the clipboard and key tools with a deadline.
//!
//! `xclip` and `wl-copy` fork a server that keeps the selection and outlives
//! the command, holding on to the stdio it inherited. So stdout is not piped
//! when nothing is read from it, and stderr goes to an unlinked temporary
//! file rather than a pipe: reading a pipe to its end would wait for the
//! server to exit.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How long a tool may run before it is killed.
pub(crate) const DEADLINE: Duration = Duration::from_secs(3);

/// Most of a tool's stderr kept for an error.
const STDERR_MAX: u64 = 4096;

/// Why a tool did not do its job.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunError {
    /// The executable is not on PATH.
    NotFound,
    /// It failed; its stderr, or what went wrong around it.
    Failed(String),
}

/// Runs `argv`, writing `input` to its stdin when given, and returns its
/// stdout when `capture` is set (empty otherwise).
pub(crate) fn run(
    argv: &[&str],
    input: Option<&str>,
    capture: bool,
    deadline: Duration,
) -> Result<String, RunError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(RunError::Failed("empty command".to_owned()));
    };
    let mut stderr = scratch().map_err(|e| RunError::Failed(format!("stderr file: {e}")))?;
    let stderr_for_child = stderr
        .try_clone()
        .map_err(|e| RunError::Failed(format!("stderr file: {e}")))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(stderr_for_child)
        .spawn()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => RunError::NotFound,
            _ => RunError::Failed(e.to_string()),
        })?;

    // Feeding and draining on threads, so a tool that neither reads nor
    // exits cannot stall us past the deadline.
    let writer = match (child.stdin.take(), input) {
        (Some(mut stdin), Some(text)) => {
            let text = text.to_owned();
            Some(thread::spawn(move || stdin.write_all(text.as_bytes())))
        }
        _ => None,
    };
    let reader = child.stdout.take().map(|mut stdout| {
        thread::spawn(move || {
            let mut out = Vec::new();
            stdout.read_to_end(&mut out).map(|_| out)
        })
    });

    let status = wait(&mut child, deadline)?;
    let written = writer.map(|w| w.join());
    let read = reader.map(|r| r.join());

    if !status.success() {
        return Err(RunError::Failed(failure(&mut stderr, status)));
    }
    if let Some(written) = written {
        match written {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(RunError::Failed(format!("writing stdin: {e}"))),
            Err(_) => return Err(RunError::Failed("stdin writer panicked".to_owned())),
        }
    }
    match read {
        None => Ok(String::new()),
        Some(Ok(Ok(out))) => Ok(String::from_utf8_lossy(&out).into_owned()),
        Some(Ok(Err(e))) => Err(RunError::Failed(format!("reading stdout: {e}"))),
        Some(Err(_)) => Err(RunError::Failed("stdout reader panicked".to_owned())),
    }
}

/// Waits for `child`, killing it at the deadline.
fn wait(child: &mut Child, deadline: Duration) -> Result<ExitStatus, RunError> {
    let until = Instant::now() + deadline;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Failed(format!(
                    "timed out after {} ms",
                    deadline.as_millis()
                )));
            }
            Err(e) => return Err(RunError::Failed(e.to_string())),
        }
    }
}

/// The tool's stderr, or its exit status when it said nothing.
fn failure(stderr: &mut File, status: ExitStatus) -> String {
    let mut said = String::new();
    let _ = stderr.rewind();
    let _ = stderr.take(STDERR_MAX).read_to_string(&mut said);
    let said = said.trim();
    if said.is_empty() {
        status.to_string()
    } else {
        said.to_owned()
    }
}

/// A fresh temporary file, already unlinked.
fn scratch() -> io::Result<File> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "anya-output-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(file)
}

/// A process holding a selection until dropped.
pub(crate) struct Owner(Child);

impl Owner {
    /// Starts `argv` and hands it `text` on stdin. The process must keep
    /// running to serve the selection; one that already failed is an error.
    pub(crate) fn start(argv: &[&str], text: &str) -> Result<Owner, RunError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(RunError::Failed("empty command".to_owned()));
        };
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => RunError::NotFound,
                _ => RunError::Failed(e.to_string()),
            })?;
        let mut owner = Owner(child);
        if let Some(mut stdin) = owner.0.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| RunError::Failed(format!("writing stdin: {e}")))?;
        }
        match owner.0.try_wait() {
            Ok(Some(status)) if !status.success() => Err(RunError::Failed(status.to_string())),
            Ok(_) => Ok(owner),
            Err(e) => Err(RunError::Failed(e.to_string())),
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHORT: Duration = Duration::from_millis(300);

    #[test]
    fn input_reaches_the_tool_and_output_comes_back() {
        assert_eq!(
            run(&["sh", "-c", "cat"], Some("你好\n world "), true, DEADLINE),
            Ok("你好\n world ".to_owned())
        );
    }

    #[test]
    fn a_missing_tool_is_reported_as_such() {
        assert_eq!(
            run(&["anya-no-such-tool"], Some("x"), false, DEADLINE),
            Err(RunError::NotFound)
        );
    }

    #[test]
    fn a_failing_tool_reports_its_stderr_or_status() {
        assert_eq!(
            run(
                &["sh", "-c", "echo boom >&2; exit 3"],
                None,
                false,
                DEADLINE
            ),
            Err(RunError::Failed("boom".to_owned()))
        );
        assert_eq!(
            run(&["sh", "-c", "exit 4"], None, false, DEADLINE),
            Err(RunError::Failed("exit status: 4".to_owned()))
        );
    }

    #[test]
    fn a_hanging_tool_is_killed_at_the_deadline() {
        let start = Instant::now();
        let r = run(&["sleep", "5"], None, false, SHORT);
        assert_eq!(
            r,
            Err(RunError::Failed("timed out after 300 ms".to_owned()))
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_server_left_behind_does_not_hold_the_call() {
        // Like xclip: read the input, leave a child with our stdio, exit 0.
        let start = Instant::now();
        let r = run(
            &["sh", "-c", "cat >/dev/null; sleep 5 & exit 0"],
            Some("text"),
            false,
            DEADLINE,
        );
        assert_eq!(r, Ok(String::new()));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn an_owner_keeps_running_until_dropped() {
        let mut owner = match Owner::start(&["sh", "-c", "cat >/dev/null; sleep 5"], "text") {
            Ok(owner) => owner,
            Err(e) => panic!("owner did not start: {e:?}"),
        };
        assert!(matches!(owner.0.try_wait(), Ok(None)));
        let pid = owner.0.id();
        drop(owner);
        assert!(!std::path::Path::new(&format!("/proc/{pid}/status")).exists());
        assert!(matches!(
            Owner::start(&["anya-no-such-tool"], "x"),
            Err(RunError::NotFound)
        ));
    }
}
