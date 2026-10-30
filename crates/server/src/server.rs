//! The server: accepts subscribers and hands each the frames it is granted.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anya_core::audio::CHUNK_MS;
use anya_core::hub::{BUFFER, Hub};
use anya_protocol::{Frame, PROTOCOL_VERSION, SUBPROTOCOL, TOKEN_SUBPROTOCOL_PREFIX};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::http::header::{
    AUTHORIZATION, HeaderValue, ORIGIN, SEC_WEBSOCKET_PROTOCOL,
};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::address::Address;
use crate::clients::Clients;
use crate::connection::{self, Live};
use crate::discovery::DiscoveryFile;
use crate::gateway::Gateway;
use crate::socket::{self, SocketFile};
use crate::tls::{TlsIdentity, lan_address};

/// How long a connection gets to finish its WebSocket handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long closing a connection may take before the socket is dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a LAN connection gets to finish its TLS handshake.
const TLS_TIMEOUT: Duration = Duration::from_secs(5);
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
    /// The TLS identity could not be made or loaded; existing files are left alone.
    #[error("TLS identity {}: {reason}", .path.display())]
    Tls {
        /// The file or directory.
        path: PathBuf,
        /// What went wrong.
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
    /// A connection without a token receives continuous transcription. With
    /// a `gateway`, paired clients may also connect with their token and
    /// dictate (docs/decisions/0009).
    pub async fn start(
        hub: Hub,
        address: Address,
        discovery: Option<PathBuf>,
        gateway: Option<Gateway>,
    ) -> Result<Server, ServerError> {
        let (version, protocol) = match hub.subscribe().snapshot.first() {
            Some(Frame::Hello {
                version, protocol, ..
            }) => (version.clone(), *protocol),
            _ => (String::new(), PROTOCOL_VERSION),
        };
        let live = Live {
            hub,
            gateway,
            lan: false,
        };
        let mut server = launch(Feed::Live(live), address).await?;
        if let Some(path) = discovery {
            let file = DiscoveryFile::write(&path, &server.url, &version, protocol)
                .map_err(|source| ServerError::Discovery { path, source })?;
            server.discovery = Some(file);
        }
        Ok(server)
    }

    /// Serves the gateway to the local network: TLS on every interface at
    /// `port` (0 for an ephemeral one), `wss://<lan address>:<port>/asr`.
    /// Every connection needs a paired client's token, except one that
    /// pairs; it receives nothing before it has one.
    pub async fn start_lan(
        hub: Hub,
        gateway: Gateway,
        port: u16,
        identity: &TlsIdentity,
    ) -> Result<Server, ServerError> {
        let live = Live {
            hub,
            gateway: Some(gateway),
            lan: true,
        };
        let any = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
        let listen_error = |source| ServerError::Listen {
            address: any.to_string(),
            source,
        };
        let listener = TcpListener::bind(any).await.map_err(listen_error)?;
        let bound = listener.local_addr().map_err(listen_error)?;
        let host = lan_address().map_or_else(|| "localhost".to_owned(), |ip| ip.to_string());
        let url = format!("wss://{host}:{}{}", bound.port(), crate::PATH);
        Ok(spawn(
            Feed::Live(live),
            Listener::Tcp(listener),
            url,
            None,
            Some(identity.acceptor()),
        ))
    }

    /// What subscribers connect to: `ws://127.0.0.1:7590/asr` with the port
    /// actually bound, `unix:<path>`, or for the LAN `wss://<address>:<port>/asr`.
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
    Live(Live),
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
    Ok(spawn(feed, listener, url, socket, None))
}

/// Starts accepting on `listener`; with `tls`, every connection is wrapped
/// in TLS first.
fn spawn(
    feed: Feed,
    listener: Listener,
    url: String,
    socket: Option<SocketFile>,
    tls: Option<TlsAcceptor>,
) -> Server {
    let subscribers = Arc::new(AtomicUsize::new(0));
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(accept(
        listener,
        feed,
        tls,
        Arc::clone(&subscribers),
        stopped,
    ));
    tracing::info!(%url, "serving subscribers");
    Server {
        url,
        subscribers,
        stop,
        task: Some(task),
        socket,
        discovery: None,
    }
}

async fn accept(
    listener: Listener,
    feed: Feed,
    tls: Option<TlsAcceptor>,
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
            Ok(Conn::Tcp(s)) => match tls.clone() {
                Some(acceptor) => {
                    conns.spawn(secure(s, acceptor, feed, subscribers, stop));
                }
                None => {
                    conns.spawn(serve(s, feed, subscribers, stop));
                }
            },
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

pub(crate) type Sink<S> = SplitSink<WebSocketStream<S>, Message>;
pub(crate) type Inbound<S> = SplitStream<WebSocketStream<S>>;

/// How a connection's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// Everything was sent (replay).
    Done,
    /// The subscriber closed or the socket broke.
    Gone,
    /// More than [`BUFFER`] frames waiting to be written.
    TooSlow,
    /// The server is stopping.
    Stopping,
    /// The client this connection belongs to was removed.
    Removed,
}

/// Finishes TLS on a LAN connection, then serves it.
async fn secure(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    feed: Feed,
    subscribers: Arc<AtomicUsize>,
    mut stop: watch::Receiver<bool>,
) {
    let tls = tokio::select! {
        _ = stop.wait_for(|s| *s) => return,
        tls = tokio::time::timeout(TLS_TIMEOUT, acceptor.accept(stream)) => match tls {
            Ok(Ok(tls)) => tls,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "TLS handshake failed");
                return;
            }
            Err(_) => {
                tracing::debug!("TLS handshake timed out");
                return;
            }
        },
    };
    serve(tls, feed, subscribers, stop).await;
}

#[allow(
    clippy::result_large_err,
    reason = "the signature of tungstenite's handshake callback"
)]
async fn serve<S>(
    stream: S,
    feed: Feed,
    subscribers: Arc<AtomicUsize>,
    mut stop: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let admission = feed.admission();
    let mut client = None;
    let handshake = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
            check_request(request, response, &admission, &mut client)
        }),
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
    let end = run(&mut sink, &mut inbound, feed, client, &mut stop).await;
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
        End::Removed => frame(CloseCode::Policy, "client removed".to_owned()),
        End::Gone => None,
    };
    close(sink, inbound, close_frame).await;
    tracing::debug!(?end, "subscriber disconnected");
}

/// What a handshake is checked against.
struct Admission {
    /// Whether a browser page from any site may connect: on the LAN the
    /// token or pairing code is the guard, not the origin.
    any_origin: bool,
    /// The paired clients whose tokens are accepted; none without a gateway.
    clients: Option<Clients>,
}

impl Feed {
    fn admission(&self) -> Admission {
        match self {
            Feed::Live(live) => Admission {
                any_origin: live.lan,
                clients: live.gateway.as_ref().map(|g| g.clients.clone()),
            },
            Feed::Replay(_) => Admission {
                any_origin: false,
                clients: None,
            },
        }
    }
}

/// Refuses everything but `/asr`; browser pages not served from loopback,
/// unless the listener accepts any origin (a page on another site could
/// otherwise read what Anya hears); and a token no paired client holds.
/// A request without `Origin` comes from a local program and is accepted.
/// The client a token names is left in `client`, and the subprotocol
/// [`SUBPROTOCOL`] is selected when offered, never the one carrying the token.
#[allow(
    clippy::result_large_err,
    reason = "the signature of tungstenite's handshake callback"
)]
fn check_request(
    request: &Request,
    mut response: Response,
    admission: &Admission,
    client: &mut Option<String>,
) -> Result<Response, ErrorResponse> {
    if request.uri().path() != crate::PATH {
        return Err(refuse(StatusCode::NOT_FOUND));
    }
    if !admission.any_origin
        && let Some(origin) = request.headers().get(ORIGIN)
        && !origin.to_str().is_ok_and(origin_is_loopback)
    {
        return Err(refuse(StatusCode::FORBIDDEN));
    }
    if let Some(token) = token_of(request) {
        let id = admission
            .clients
            .as_ref()
            .and_then(|clients| clients.authenticate(&token));
        *client = Some(id.ok_or_else(|| refuse(StatusCode::UNAUTHORIZED))?);
    }
    if subprotocols(request).any(|p| p == SUBPROTOCOL) {
        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(SUBPROTOCOL),
        );
    }
    Ok(response)
}

/// The subprotocols a request offers.
fn subprotocols(request: &Request) -> impl Iterator<Item = &str> {
    request
        .headers()
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .into_iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
}

/// The token a request carries: `Authorization: Bearer <token>`, or a
/// subprotocol `anya.token.<token>`.
fn token_of(request: &Request) -> Option<String> {
    let bearer = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim().to_owned());
    bearer.or_else(|| {
        subprotocols(request)
            .find_map(|p| p.strip_prefix(TOKEN_SUBPROTOCOL_PREFIX))
            .map(str::to_owned)
    })
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
    client: Option<String>,
    stop: &mut watch::Receiver<bool>,
) -> End
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let frames = match feed {
        Feed::Live(live) => return connection::run(sink, inbound, live, client, stop).await,
        Feed::Replay(frames) => frames,
    };
    let (queue, queued) = mpsc::channel(BUFFER);
    let feeder = replay(frames, queue);
    tokio::pin!(feeder);
    let writer = write_queue(sink, queued);
    tokio::pin!(writer);
    loop {
        tokio::select! {
            biased;
            _ = stop.wait_for(|s| *s) => return End::Stopping,
            end = &mut feeder => return end,
            end = &mut writer => return end,
            // A replay only plays: whatever a subscriber sends is read and ignored.
            message = inbound.next() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return End::Gone,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Writes queued messages, flushing after each burst.
pub(crate) async fn write_queue<S>(sink: &mut Sink<S>, mut queued: mpsc::Receiver<Message>) -> End
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

pub(crate) fn encode(frame: &Frame) -> Option<Message> {
    match serde_json::to_string(frame) {
        Ok(json) => Some(Message::text(json)),
        Err(e) => {
            tracing::warn!(error = %e, "a frame could not be encoded");
            None
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
