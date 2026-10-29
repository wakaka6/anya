//! The client behind `anya subscribe`: one connection, each frame printed
//! as a line of NDJSON, until a limit, a timeout, a signal or the server
//! ends it.

use std::fmt;
use std::io;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::address::Address;

/// How long closing the connection may take on the way out.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// Why [`subscribe`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// `max_events` frames were received.
    Limit,
    /// The timeout passed.
    Timeout,
    /// SIGINT or SIGTERM arrived.
    Signal,
    /// The server closed the connection, reading failed, or stdout did.
    Closed,
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ExitReason::Limit => "limit",
            ExitReason::Timeout => "timeout",
            ExitReason::Signal => "signal",
            ExitReason::Closed => "closed",
        })
    }
}

/// How a subscription ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscribeExit {
    /// Why it ended.
    pub reason: ExitReason,
    /// Frames written to stdout.
    pub frames: u64,
    /// Time since connecting; since the start when it never connected.
    pub elapsed: Duration,
    /// Whether the connection was made: a timeout or a signal can come first.
    pub connected: bool,
}

/// Why [`subscribe`] failed. Display text is English, for logs.
#[derive(Debug, thiserror::Error)]
pub enum SubscribeError {
    /// Nothing answered at the address, or the WebSocket handshake failed.
    #[error("cannot connect to {address}: {source}; is Anya running, and listening there?")]
    Connect {
        /// The address, as printed on the ready line.
        address: String,
        /// What went wrong.
        source: io::Error,
    },
}

/// Connects to `address` and writes each frame to `stdout` as one line.
/// On `stderr`: `[subscribe] ready addr=…` once connected and, on the way
/// out, `[subscribe] exited — received N frame(s) in Xs (reason: …)`.
/// `max_events` and `timeout` both bound the run, the timeout including the
/// connect; `Some(0)` of either means no bound. A timeout or a signal before
/// the connection is made ends the run quietly.
///
/// Registers SIGINT and SIGTERM handlers for the rest of the process: from
/// then on these signals no longer terminate it by default.
pub async fn subscribe(
    address: Address,
    max_events: Option<u64>,
    timeout: Option<Duration>,
    stdout: impl AsyncWrite,
    stderr: impl AsyncWrite,
) -> Result<SubscribeExit, SubscribeError> {
    let start = Instant::now();
    let limit = max_events.filter(|n| *n > 0);
    let deadline = timeout.filter(|t| !t.is_zero()).map(|t| start + t);
    let signal = shutdown();
    let expiry = expire(deadline);
    tokio::pin!(stdout, stderr, signal, expiry);
    let quiet = |reason| {
        Ok(SubscribeExit {
            reason,
            frames: 0,
            elapsed: start.elapsed(),
            connected: false,
        })
    };
    let mut ws = tokio::select! {
        biased;
        () = &mut signal => return quiet(ExitReason::Signal),
        () = &mut expiry => return quiet(ExitReason::Timeout),
        ws = connect(&address) => ws.map_err(|source| SubscribeError::Connect {
            address: address.url(),
            source,
        })?,
    };
    let ready = Instant::now();
    let line = format!("[subscribe] ready addr={}\n", address.url());
    let _ = write_line(&mut stderr, line.as_bytes()).await;
    let mut frames = 0u64;
    let reason = 'read: loop {
        if limit.is_some_and(|n| frames >= n) {
            break ExitReason::Limit;
        }
        let text = tokio::select! {
            biased;
            () = &mut signal => break ExitReason::Signal,
            () = &mut expiry => break ExitReason::Timeout,
            message = ws.next() => match message {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(_)) | Err(_)) | None => break ExitReason::Closed,
                Some(Ok(_)) => continue,
            },
        };
        let mut line = Vec::with_capacity(text.len() + 1);
        line.extend_from_slice(text.as_bytes());
        line.push(b'\n');
        // A reader that stopped reading the pipe must not hold off a signal.
        let written = tokio::select! {
            biased;
            () = &mut signal => break 'read ExitReason::Signal,
            () = &mut expiry => break 'read ExitReason::Timeout,
            written = write_line(&mut stdout, &line) => written,
        };
        if written.is_err() {
            break ExitReason::Closed;
        }
        frames += 1;
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close(None)).await;
    let elapsed = ready.elapsed();
    let line = format!(
        "[subscribe] exited — received {frames} frame(s) in {:.3}s (reason: {reason})\n",
        elapsed.as_secs_f64()
    );
    let _ = write_line(&mut stderr, line.as_bytes()).await;
    Ok(SubscribeExit {
        reason,
        frames,
        elapsed,
        connected: true,
    })
}

async fn write_line<W: AsyncWrite + Unpin>(out: &mut W, line: &[u8]) -> io::Result<()> {
    out.write_all(line).await?;
    out.flush().await
}

/// Resolves on SIGINT or SIGTERM. The handlers are registered when this is
/// called, not when it is first polled.
fn shutdown() -> impl Future<Output = ()> {
    let mut interrupt = signal(SignalKind::interrupt()).ok();
    let mut terminate = signal(SignalKind::terminate()).ok();
    async move {
        tokio::select! {
            () = received(&mut interrupt) => {}
            () = received(&mut terminate) => {}
        }
    }
}

async fn received(signal: &mut Option<Signal>) {
    if let Some(signal) = signal
        && signal.recv().await.is_some()
    {
        return;
    }
    std::future::pending().await
}

async fn expire(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn connect(address: &Address) -> io::Result<WebSocketStream<Box<dyn Io>>> {
    let (stream, url): (Box<dyn Io>, String) = match address {
        Address::Tcp(addr) => (Box::new(TcpStream::connect(addr).await?), address.url()),
        // The host is not used on a socket; the request still needs one.
        Address::Unix(path) => (
            Box::new(UnixStream::connect(path).await?),
            format!("ws://localhost{}", crate::PATH),
        ),
    };
    let (ws, _) = tokio_tungstenite::client_async(url, stream)
        .await
        .map_err(|e| match e {
            tungstenite::Error::Io(e) => e,
            e => io::Error::other(e.to_string()),
        })?;
    Ok(ws)
}
