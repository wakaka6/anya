//! The server: accepts subscribers and hands each the frames it is granted.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anya_core::audio::CHUNK_MS;
use anya_core::hub::{BUFFER, Hub};
use anya_protocol::{Frame, PROTOCOL_VERSION, SessionId, Source};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::http::header::ORIGIN;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::address::Address;
use crate::discovery::DiscoveryFile;
use crate::socket::{self, SocketFile};

/// How long a connection gets to finish its WebSocket handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long closing a connection may take before the socket is dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Messages written before the socket is flushed.
const BATCH: usize = 64;
/// Realtime replay: the pause before each `result`.
const RESULT_GAP: Duration = Duration::from_millis(500);
/// Realtime replay: the pause before every other frame after the preamble.
const STEP: Duration = Duration::from_millis(200);

/// Why the server did not start. Display text is English, for logs; the app
/// matches on the variant to show its own words.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Binding the address failed: taken, or not allowed.
    #[error("listen {address}: {source}")]
    Listen {
        /// The address as configured.
        address: String,
        /// The system's error.
        source: io::Error,
    },
    /// The socket path holds something other than a socket; it is left alone.
    #[error("{} exists and is not a socket; it is left alone", .0.display())]
    NotASocket(PathBuf),
    /// Another instance already listens on the socket.
    #[error("another instance is already listening on {0}")]
    InUse(String),
    /// The socket's directory does not exist; it is not created.
    #[error("the directory {} does not exist", .0.display())]
    MissingDirectory(PathBuf),
    /// The socket could not be made owner-only, so it was not served.
    #[error("chmod {}: {source}", .path.display())]
    Permissions {
        /// The socket.
        path: PathBuf,
        /// The system's error.
        source: io::Error,
    },
    /// The discovery file could not be written.
    #[error("writing the discovery file {}: {source}", .path.display())]
    Discovery {
        /// The discovery file.
        path: PathBuf,
        /// The system's error.
        source: io::Error,
    },
    /// A replay line is not a JSON object; nothing was served.
    #[error("replay line {line}: {reason}")]
    Replay {
        /// The line, counting from 1.
        line: usize,
        /// Why it is not a frame.
        reason: String,
    },
}

/// A running server. Dropping it stops accepting and disconnects every
/// subscriber in the background; [`Server::stop`] waits until they are gone.
pub struct Server {
    url: String,
    subscribers: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    socket: Option<SocketFile>,
    discovery: Option<DiscoveryFile>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("url", &self.url)
            .field("subscribers", &self.subscribers())
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Serves `hub`'s frames on `address`. With `discovery`, writes
    /// `{url, version, protocol, pid}` there (0600) and removes it on stop.
    pub async fn start(
        hub: Hub,
        address: Address,
        discovery: Option<PathBuf>,
    ) -> Result<Server, ServerError> {
        let (version, protocol) = match hub.subscribe().snapshot.first() {
            Some(Frame::Hello {
                version, protocol, ..
            }) => (version.clone(), *protocol),
            _ => (String::new(), PROTOCOL_VERSION),
        };
        let mut server = launch(Feed::Live(hub), address).await?;
        if let Some(path) = discovery {
            let file = DiscoveryFile::write(&path, &server.url, &version, protocol)
                .map_err(|source| ServerError::Discovery { path, source })?;
            server.discovery = Some(file);
        }
        Ok(server)
    }

    /// What subscribers connect to: `ws://127.0.0.1:7590/asr` with the port
    /// actually bound, or `unix:<path>`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Subscribers connected now and still receiving frames.
    pub fn subscribers(&self) -> usize {
        self.subscribers.load(Ordering::SeqCst)
    }

    /// Stops listening, closes every subscriber with "going away", waits
    /// for them, and removes the socket and discovery files.
    pub async fn stop(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        self.remove_files();
    }

    fn remove_files(&mut self) {
        if let Some(socket) = self.socket.take() {
            socket.remove();
        }
        if let Some(discovery) = self.discovery.take() {
            discovery.remove();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.remove_files();
    }
}

/// Serves `lines`, one frame each, to every connection, from the first, then
/// closes it normally with "end of replay". Each line goes out exactly as
/// given, so frames from a newer Anya arrive unchanged; a line that is not a
/// JSON object is refused before anything listens. With `realtime`, frames
/// are paced like a live session by their `type`: the leading `hello` and
/// `source` frames at once, then 40 ms before each `level`, 500 ms before
/// each `result` and 200 ms before anything else; otherwise all at once.
pub async fn start_replay(
    lines: Vec<String>,
    address: Address,
    realtime: bool,
) -> Result<Server, ServerError> {
    let mut preamble = true;
    let mut paced = Vec::with_capacity(lines.len());
    for (index, line) in lines.into_iter().enumerate() {
        let object: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&line)
            .map_err(|e| ServerError::Replay {
                line: index + 1,
                reason: e.to_string(),
            })?;
        let kind = object.get("type").and_then(serde_json::Value::as_str);
        let gap = pace(kind, &mut preamble);
        let gap = if realtime { gap } else { Duration::ZERO };
        paced.push((Message::text(line), gap));
    }
    launch(Feed::Replay(paced.into()), address).await
}

/// The pause before a frame of type `kind`; unknown kinds pace as anything else.
fn pace(kind: Option<&str>, preamble: &mut bool) -> Duration {
    if *preamble && matches!(kind, Some("hello" | "source")) {
        return Duration::ZERO;
    }
    *preamble = false;
    match kind {
        Some("level") => Duration::from_millis(CHUNK_MS.into()),
        Some("result") => RESULT_GAP,
        _ => STEP,
    }
}

/// What each connection is sent.
#[derive(Clone)]
enum Feed {
    /// The hub's snapshot, then its live frames.
    Live(Hub),
    /// Fixed messages, each after its pause.
    Replay(Arc<[(Message, Duration)]>),
}

enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

enum Conn {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Listener {
    async fn accept(&self) -> io::Result<Conn> {
        match self {
            Listener::Tcp(l) => l.accept().await.map(|(s, _)| Conn::Tcp(s)),
            Listener::Unix(l) => l.accept().await.map(|(s, _)| Conn::Unix(s)),
        }
    }
}

async fn launch(feed: Feed, address: Address) -> Result<Server, ServerError> {
    let (listener, url, socket) = match &address {
        Address::Tcp(addr) => {
            let listen_error = |source| ServerError::Listen {
                address: address.to_string(),
                source,
            };
            let listener = TcpListener::bind(addr).await.map_err(listen_error)?;
            let bound = listener.local_addr().map_err(listen_error)?;
            (Listener::Tcp(listener), Address::Tcp(bound).url(), None)
        }
        Address::Unix(path) => {
            let (listener, file) = socket::bind(path).await?;
            (Listener::Unix(listener), address.url(), Some(file))
        }
    };
    let subscribers = Arc::new(AtomicUsize::new(0));
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(accept(listener, feed, Arc::clone(&subscribers), stopped));
    tracing::info!(%url, "serving subscribers");
    Ok(Server {
        url,
        subscribers,
        stop,
        task: Some(task),
        socket,
        discovery: None,
    })
}

async fn accept(
    listener: Listener,
    feed: Feed,
    subscribers: Arc<AtomicUsize>,
    mut stop: watch::Receiver<bool>,
) {
    let mut conns = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            biased;
            _ = stop.wait_for(|s| *s) => break,
            accepted = listener.accept() => accepted,
            Some(_) = conns.join_next(), if !conns.is_empty() => continue,
        };
        let (feed, subscribers, stop) = (feed.clone(), Arc::clone(&subscribers), stop.clone());
        match accepted {
            Ok(Conn::Tcp(s)) => {
                conns.spawn(serve(s, feed, subscribers, stop));
            }
            Ok(Conn::Unix(s)) => {
                conns.spawn(serve(s, feed, subscribers, stop));
            }
            Err(e) => {
                // Out of descriptors and the like: wait instead of spinning.
                tracing::warn!(error = %e, "accepting a subscriber failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    drop(listener);
    while conns.join_next().await.is_some() {}
}

/// Counts a subscriber while it is being sent frames.
struct Counted {
    count: Arc<AtomicUsize>,
    held: bool,
}

impl Counted {
    fn new(count: Arc<AtomicUsize>) -> Counted {
        count.fetch_add(1, Ordering::SeqCst);
        Counted { count, held: true }
    }

    fn release(&mut self) {
        if std::mem::take(&mut self.held) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.release();
    }
}

type Sink<S> = SplitSink<WebSocketStream<S>, Message>;
type Inbound<S> = SplitStream<WebSocketStream<S>>;

/// How a connection's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Everything was sent (replay).
    Done,
    /// The subscriber closed or the socket broke.
    Gone,
    /// More than [`BUFFER`] frames waiting to be written.
    TooSlow,
    /// The server is stopping.
    Stopping,
}

async fn serve<S>(
    stream: S,
    feed: Feed,
    subscribers: Arc<AtomicUsize>,
    mut stop: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let handshake = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::accept_hdr_async(stream, check_request),
    );
    let ws = tokio::select! {
        _ = stop.wait_for(|s| *s) => return,
        handshake = handshake => match handshake {
            Ok(Ok(ws)) => ws,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "subscriber handshake refused");
                return;
            }
            Err(_) => {
                tracing::debug!("subscriber handshake timed out");
                return;
            }
        },
    };
    let mut counted = Counted::new(subscribers);
    tracing::debug!("subscriber connected");
    let (mut sink, mut inbound) = ws.split();
    let end = run(&mut sink, &mut inbound, feed, &mut stop).await;
    // From here on the subscriber receives nothing more.
    counted.release();
    let frame = |code, reason: String| {
        Some(CloseFrame {
            code,
            reason: reason.into(),
        })
    };
    let close_frame = match end {
        End::TooSlow => frame(
            CloseCode::Policy,
            format!("too slow: more than {BUFFER} frames behind"),
        ),
        End::Stopping => frame(CloseCode::Away, "Anya is stopping".to_owned()),
        End::Done => frame(CloseCode::Normal, "end of replay".to_owned()),
        End::Gone => None,
    };
    close(sink, inbound, close_frame).await;
    tracing::debug!(?end, "subscriber disconnected");
}

/// Refuses everything but `/asr`, and browser pages not served from
/// loopback: a page on another site could otherwise read what Anya hears.
/// A request without `Origin` comes from a local program and is accepted.
#[allow(
    clippy::result_large_err,
    reason = "the signature of tungstenite's handshake callback"
)]
fn check_request(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    if request.uri().path() != crate::PATH {
        return Err(refuse(StatusCode::NOT_FOUND));
    }
    if let Some(origin) = request.headers().get(ORIGIN)
        && !origin.to_str().is_ok_and(origin_is_loopback)
    {
        return Err(refuse(StatusCode::FORBIDDEN));
    }
    Ok(response)
}

fn refuse(status: StatusCode) -> ErrorResponse {
    let mut response = ErrorResponse::new(status.canonical_reason().map(str::to_owned));
    *response.status_mut() = status;
    response
}

/// An origin whose host is `localhost`, a `*.localhost` name or a loopback
/// IP, under any scheme (an embedded webview's own scheme included). `null`
/// (sandboxed frames, `file:` pages) is refused.
fn origin_is_loopback(origin: &str) -> bool {
    let Some((_, rest)) = origin.split_once("://") else {
        return false;
    };
    let host = match rest.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => rest.split([':', '/']).next().unwrap_or_default(),
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

async fn run<S>(
    sink: &mut Sink<S>,
    inbound: &mut Inbound<S>,
    feed: Feed,
    stop: &mut watch::Receiver<bool>,
) -> End
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (mut feeder, queued): (Pin<Box<dyn Future<Output = End> + Send>>, _) = match feed {
        Feed::Live(hub) => {
            let subscription = hub.subscribe();
            let mut grant = Grant::default();
            let snapshot: Vec<Message> = subscription
                .snapshot
                .iter()
                .filter(|f| grant.admits(f))
                .filter_map(encode)
                .collect();
            // Room for the snapshot on top of the frames a subscriber may
            // fall behind (docs/protocol.md, promise 10).
            let (queue, queued) = mpsc::channel(BUFFER + snapshot.len());
            for message in snapshot {
                let _ = queue.try_send(message);
            }
            (Box::pin(live(subscription.frames, grant, queue)), queued)
        }
        Feed::Replay(frames) => {
            let (queue, queued) = mpsc::channel(BUFFER);
            (Box::pin(replay(frames, queue)), queued)
        }
    };
    let writer = write_queue(sink, queued);
    tokio::pin!(writer);
    loop {
        tokio::select! {
            biased;
            _ = stop.wait_for(|s| *s) => return End::Stopping,
            end = &mut feeder => return end,
            end = &mut writer => return end,
            // Subscribers only listen: whatever they send is read and ignored.
            message = inbound.next() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return End::Gone,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Writes queued messages, flushing after each burst.
async fn write_queue<S>(sink: &mut Sink<S>, mut queued: mpsc::Receiver<Message>) -> End
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(message) = queued.recv().await {
        if sink.feed(message).await.is_err() {
            return End::Gone;
        }
        let mut written = 1;
        while written < BATCH
            && let Ok(message) = queued.try_recv()
        {
            if sink.feed(message).await.is_err() {
                return End::Gone;
            }
            written += 1;
        }
        if sink.flush().await.is_err() {
            return End::Gone;
        }
    }
    End::Done
}

/// Moves granted live frames to the write queue. Never waits on the
/// subscriber: a full queue means it is more than [`BUFFER`] frames behind.
async fn live(
    mut frames: broadcast::Receiver<Frame>,
    mut grant: Grant,
    queue: mpsc::Sender<Message>,
) -> End {
    loop {
        match frames.recv().await {
            Ok(frame) => {
                if !grant.admits(&frame) {
                    continue;
                }
                let Some(message) = encode(&frame) else {
                    continue;
                };
                match queue.try_send(message) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => return End::TooSlow,
                    Err(TrySendError::Closed(_)) => return End::Gone,
                }
            }
            Err(RecvError::Lagged(_)) => return End::TooSlow,
            Err(RecvError::Closed) => return End::Stopping,
        }
    }
}

/// Queues the replay's messages after their pauses, then lets the writer
/// drain the queue.
async fn replay(frames: Arc<[(Message, Duration)]>, queue: mpsc::Sender<Message>) -> End {
    for (message, gap) in frames.iter() {
        if !gap.is_zero() {
            tokio::time::sleep(*gap).await;
        }
        if queue.send(message.clone()).await.is_err() {
            return End::Gone;
        }
    }
    drop(queue);
    std::future::pending().await
}

/// Sends the close frame and waits for the subscriber's reply, or flushes
/// the reply to a close the subscriber started; bounded by [`CLOSE_TIMEOUT`].
async fn close<S>(mut sink: Sink<S>, mut inbound: Inbound<S>, frame: Option<CloseFrame>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let closing = async {
        match frame {
            Some(frame) => {
                if sink.send(Message::Close(Some(frame))).await.is_ok() {
                    while let Some(Ok(_)) = inbound.next().await {}
                }
            }
            None => {
                let _ = sink.close().await;
            }
        }
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, closing).await;
}

fn encode(frame: &Frame) -> Option<Message> {
    match serde_json::to_string(frame) {
        Ok(json) => Some(Message::text(json)),
        Err(e) => {
            tracing::warn!(error = %e, "a frame could not be encoded");
            None
        }
    }
}

/// The sources a subscriber receives, and its sessions of those sources.
///
/// Every subscriber is granted `continuous` only: dictation sessions, their
/// levels and the dictation source never leave. How an app identifies itself
/// and gets the user's grant for dictation is still open
/// (docs/decisions/0007-downstream-apps.md, "Open"); the grant is per
/// subscriber so that decision lands here.
#[derive(Default)]
struct Grant {
    sessions: HashSet<SessionId>,
}

impl Grant {
    fn granted(source: Source) -> bool {
        source == Source::Continuous
    }

    fn admits(&mut self, frame: &Frame) -> bool {
        match frame {
            Frame::Hello { .. } => true,
            Frame::Source { source, .. } => Grant::granted(*source),
            Frame::SessionStart {
                session, source, ..
            } => Grant::granted(*source) && self.sessions.insert(session.clone()),
            Frame::Result { session, .. } | Frame::Level { session, .. } => {
                self.sessions.contains(session)
            }
            Frame::SessionEnd { session, .. } => self.sessions.remove(session),
            Frame::Unknown => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::origin_is_loopback;

    #[test]
    fn only_loopback_origins_pass() {
        for ok in [
            "http://localhost",
            "http://LOCALHOST:5173",
            "https://127.0.0.1:8443",
            "http://127.0.0.2",
            "http://[::1]:3000",
            "tauri://localhost",
            "http://tauri.localhost",
        ] {
            assert!(origin_is_loopback(ok), "{ok}");
        }
        for refused in [
            "null",
            "https://example.com",
            "http://localhost.example.com",
            "http://192.168.1.2:7590",
            "http://[::]:80",
            "http://0.0.0.0",
            "",
        ] {
            assert!(!origin_is_loopback(refused), "{refused}");
        }
    }
}
