//! What the server tests share: a hub with a scripted engine, clients over
//! real sockets, fixtures and temporary directories.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "a module shared by test binaries, each using a part of it"
)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anya_core::audio::{CHUNK_BYTES, Chunk};
use anya_core::engine::{Engine, Input, Output, Session, SessionParams};
use anya_core::hub::{Hub, HubInfo, LiveSession};
use anya_protocol::{Frame, Manifest, Recognition, Source, Utterance};
use anya_server::{Address, Server, parse_address};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{self, Message};

/// How long any single step may take before the test fails.
pub const WAIT: Duration = Duration::from_secs(10);

pub fn hub() -> Hub {
    Hub::new(HubInfo {
        version: "0.1.0-test".to_owned(),
        session_prefix: "test".to_owned(),
    })
}

/// An engine the test drives: each session's output is the test's to send.
pub struct FakeEngine {
    manifest: Manifest,
    sessions: Mutex<Vec<(mpsc::Sender<Output>, mpsc::Receiver<Input>)>>,
}

impl FakeEngine {
    pub fn new() -> FakeEngine {
        FakeEngine {
            manifest: Manifest {
                id: "fake".to_owned(),
                title: "Fake".into(),
                blurb: "".into(),
                docs: None,
                streaming: true,
                fields: Vec::new(),
            },
            sessions: Mutex::new(Vec::new()),
        }
    }
}

impl Engine for FakeEngine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, _params: SessionParams) -> Session {
        let (input, inputs) = mpsc::channel(64);
        let (outputs, output) = mpsc::channel(64);
        self.sessions
            .lock()
            .expect("sessions")
            .push((outputs, inputs));
        Session { input, output }
    }
}

/// A started session and the engine end that reports its results.
pub struct Running {
    pub hub: Hub,
    pub live: LiveSession,
    out: mpsc::Sender<Output>,
}

/// Opens a session of `source` and waits until its `session_start` is out.
pub async fn open(hub: &Hub, engine: &FakeEngine, source: Source) -> Running {
    let mut tap = hub.subscribe().frames;
    let live = hub.open(engine, source, SessionParams::default());
    let out = engine
        .sessions
        .lock()
        .expect("sessions")
        .last()
        .expect("a session")
        .0
        .clone();
    out.send(Output::Ready).await.expect("ready");
    let id = live.id().clone();
    until(
        &mut tap,
        |f| matches!(f, Frame::SessionStart { session, .. } if *session == id),
    )
    .await;
    Running {
        hub: hub.clone(),
        live,
        out,
    }
}

impl Running {
    /// Reports `recognition` and waits until the hub published it.
    pub async fn push(&self, recognition: Recognition) {
        let mut tap = self.hub.subscribe().frames;
        let text = recognition.text.clone();
        self.out
            .send(Output::Result(recognition))
            .await
            .expect("result");
        let id = self.live.id().clone();
        until(&mut tap, |f| {
            matches!(f, Frame::Result { session, recognition } if *session == id && recognition.text == text)
        })
        .await;
    }

    /// Sends one chunk of audio: the hub publishes its `level`.
    pub async fn level(&self) {
        assert!(self.live.send(Chunk(vec![0; CHUNK_BYTES])).await);
    }
}

/// A recognition whose text is its utterances joined.
pub fn rec(utterances: &[(&str, bool)], is_final: bool) -> Recognition {
    let utterances: Vec<Utterance> = utterances
        .iter()
        .map(|(text, definite)| Utterance {
            text: (*text).to_owned(),
            definite: *definite,
        })
        .collect();
    Recognition {
        text: utterances.iter().map(|u| u.text.as_str()).collect(),
        utterances,
        is_final,
    }
}

async fn until(tap: &mut broadcast::Receiver<Frame>, matches: impl Fn(&Frame) -> bool) {
    tokio::time::timeout(WAIT, async {
        loop {
            match tap.recv().await {
                Ok(frame) if matches(&frame) => return,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("hub closed"),
            }
        }
    })
    .await
    .expect("the hub published the frame");
}

/// Waits until `cond` holds.
pub async fn eventually(mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the condition held in time");
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub type Client = WebSocketStream<Box<dyn Io>>;

/// The address a server's URL names.
pub fn address_of(server: &Server) -> Address {
    let url = server.url();
    let addr = url
        .strip_prefix("ws://")
        .and_then(|r| r.strip_suffix("/asr"))
        .unwrap_or(&url);
    parse_address(addr).expect("the server's own address")
}

/// Connects to `server_url` at `path`, sending `origin` when given.
pub async fn connect_with(
    server_url: &str,
    path: &str,
    origin: Option<&str>,
) -> Result<Client, tungstenite::Error> {
    let (stream, host): (Box<dyn Io>, String) = match server_url.strip_prefix("unix:") {
        Some(socket) => (
            Box::new(UnixStream::connect(socket).await?),
            "localhost".to_owned(),
        ),
        None => {
            let addr = server_url
                .strip_prefix("ws://")
                .and_then(|r| r.strip_suffix("/asr"))
                .expect("a ws:// url");
            (Box::new(TcpStream::connect(addr).await?), addr.to_owned())
        }
    };
    let mut request = format!("ws://{host}{path}").into_client_request()?;
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().expect("a header value"));
    }
    let (ws, _) = tokio_tungstenite::client_async(request, stream).await?;
    Ok(ws)
}

pub async fn connect(server: &Server) -> Client {
    connect_with(&server.url(), "/asr", None)
        .await
        .expect("connect")
}

/// The next text message; pings and pongs are skipped.
pub async fn next_text(client: &mut Client) -> String {
    tokio::time::timeout(WAIT, async {
        loop {
            match client.next().await {
                Some(Ok(Message::Text(text))) => return text.as_str().to_owned(),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                other => panic!("expected a frame, got {other:?}"),
            }
        }
    })
    .await
    .expect("a frame in time")
}

pub async fn next_frame(client: &mut Client) -> Frame {
    let text = next_text(client).await;
    serde_json::from_str(&text).expect("a frame")
}

pub async fn frames(client: &mut Client, n: usize) -> Vec<Frame> {
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        frames.push(next_frame(client).await);
    }
    frames
}

/// Every text message until the server closes, and its close frame. Reads
/// on to the end so the close reply goes out.
pub async fn until_close(client: &mut Client) -> (Vec<String>, Option<CloseFrame>) {
    tokio::time::timeout(WAIT, async {
        let mut texts = Vec::new();
        loop {
            match client.next().await {
                Some(Ok(Message::Text(text))) => texts.push(text.as_str().to_owned()),
                Some(Ok(Message::Close(frame))) => {
                    while let Some(Ok(_)) = client.next().await {}
                    return (texts, frame);
                }
                Some(Ok(_)) => {}
                other => panic!("expected a close frame, got {other:?}"),
            }
        }
    })
    .await
    .expect("closed in time")
}

/// A fixture's lines.
pub fn fixture(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../protocol/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("read fixture");
    text.lines().map(str::to_owned).collect()
}

/// A directory removed when dropped, short enough for a socket path.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("anya-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub use futures_util::{SinkExt, StreamExt};
