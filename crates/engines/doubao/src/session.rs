//! One streaming session: connect, send the parameters, stream audio in
//! packets, read results until the service's last packet.

use std::time::Duration;

use anya_core::engine::{EngineError, Input, Output, Session, SessionParams};
use anya_protocol::Recognition;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message as WsMessage};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::config::{Auth, Config};
use crate::frame::{FULL_SERVER_RESPONSE, Message, Packet};
use crate::transcript::Transcript;

/// Audio per packet: 200 ms, the size the service recommends.
const PACKET_BYTES: usize = 6400;
/// Chunks a session takes before the sender waits. Audio that arrives while
/// connecting is moved into the session's own buffer, so this stays small.
const INPUT_CAPACITY: usize = 64;
const OUTPUT_CAPACITY: usize = 64;
/// How long a closing session waits to hand its close frame over.
const CLOSE_WAIT: Duration = Duration::from_millis(500);
/// How much of a rejected handshake's body goes into the error's detail.
const BODY_CHARS: usize = 300;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) fn open(params: SessionParams) -> Session {
    let (input, inputs) = mpsc::channel(INPUT_CAPACITY);
    let (results, output) = mpsc::channel(OUTPUT_CAPACITY);
    tokio::spawn(async move {
        if let Err(e) = run(params, inputs, &results).await {
            tracing::warn!(error = %e, "doubao session failed");
            let _ = results.send(Output::Error(e)).await;
        }
    });
    Session { input, output }
}

async fn run(
    params: SessionParams,
    mut inputs: mpsc::Receiver<Input>,
    results: &mpsc::Sender<Output>,
) -> Result<(), EngineError> {
    let config = Config::parse(&params.settings, params.language.as_deref())?;
    let request = handshake(&config)?;

    // Audio keeps arriving while the connection is made; hold it.
    let mut pending = Vec::new();
    let mut finishing = false;
    let connect = tokio_tungstenite::connect_async(request);
    tokio::pin!(connect);
    let (mut ws, response) = loop {
        tokio::select! {
            connected = &mut connect => break connected.map_err(connect_error)?,
            input = inputs.recv(), if !finishing => match input {
                Some(Input::Audio(chunk)) => pending.extend_from_slice(&chunk.0),
                Some(Input::Finish) => finishing = true,
                None => return Ok(()),
            },
            () = results.closed() => return Ok(()),
        }
    };
    let logid = logid(response.headers());
    tracing::info!(logid = %logid, resource = %config.resource_id, "doubao connected");

    let init = config.request(&params.hotwords).to_string().into_bytes();
    send(&mut ws, Packet::full_client_request(init)).await?;
    if results.send(Output::Ready).await.is_err() {
        close(&mut ws).await;
        return Ok(());
    }

    let mut audio = Audio {
        pending,
        sent: false,
    };
    if finishing {
        if !audio.finish(&mut ws).await? {
            return end_silent(&mut ws, results).await;
        }
    } else {
        audio.flush(&mut ws).await?;
    }

    let mut transcript = Transcript::default();
    loop {
        tokio::select! {
            input = inputs.recv(), if !finishing => match input {
                Some(Input::Audio(chunk)) => {
                    audio.pending.extend_from_slice(&chunk.0);
                    audio.flush(&mut ws).await?;
                }
                Some(Input::Finish) => {
                    finishing = true;
                    if !audio.finish(&mut ws).await? {
                        return end_silent(&mut ws, results).await;
                    }
                }
                None => {
                    close(&mut ws).await;
                    return Ok(());
                }
            },
            frame = ws.next() => match frame {
                Some(Ok(WsMessage::Binary(bytes))) => match Message::decode(&bytes) {
                    Ok(Message::Packet(p)) if p.message == FULL_SERVER_RESPONSE => {
                        let Some(recognition) = transcript.apply(&p.payload, p.last) else {
                            continue;
                        };
                        let is_final = recognition.is_final;
                        if results.send(Output::Result(recognition)).await.is_err() || is_final {
                            close(&mut ws).await;
                            return Ok(());
                        }
                    }
                    // A message type this session does not use.
                    Ok(Message::Packet(_)) => {}
                    Ok(Message::Error { code, message }) => {
                        return Err(EngineError::Upstream(with_logid(
                            format!("{code}: {message}"),
                            &logid,
                        )));
                    }
                    Err(e) => tracing::debug!(error = %e, "skipped an undecodable frame"),
                },
                Some(Ok(WsMessage::Close(frame))) => {
                    let detail = match frame {
                        Some(f) if !f.reason.is_empty() => {
                            format!("closed by the service ({}): {}", u16::from(f.code), f.reason)
                        }
                        Some(f) => format!("closed by the service ({})", u16::from(f.code)),
                        None => "closed by the service".to_owned(),
                    };
                    return Err(EngineError::Upstream(with_logid(detail, &logid)));
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(EngineError::Connect(with_logid(e.to_string(), &logid))),
                None => {
                    return Err(EngineError::Upstream(with_logid(
                        "the connection ended before the final result".to_owned(),
                        &logid,
                    )));
                }
            },
            () = results.closed() => {
                close(&mut ws).await;
                return Ok(());
            }
        }
    }
}

/// Audio not yet sent, and whether any was.
struct Audio {
    pending: Vec<u8>,
    sent: bool,
}

impl Audio {
    /// Sends every whole packet.
    async fn flush(&mut self, ws: &mut Socket) -> Result<(), EngineError> {
        while self.pending.len() >= PACKET_BYTES {
            self.packet(ws, PACKET_BYTES, false).await?;
        }
        Ok(())
    }

    /// Sends the rest, the last packet flagged last. False when the session
    /// had no audio at all: nothing is sent, as the service rejects empty
    /// audio.
    async fn finish(&mut self, ws: &mut Socket) -> Result<bool, EngineError> {
        if !self.sent && self.pending.is_empty() {
            return Ok(false);
        }
        while self.pending.len() > PACKET_BYTES {
            self.packet(ws, PACKET_BYTES, false).await?;
        }
        self.packet(ws, self.pending.len(), true).await?;
        Ok(true)
    }

    async fn packet(&mut self, ws: &mut Socket, len: usize, last: bool) -> Result<(), EngineError> {
        let rest = self.pending.split_off(len);
        let pcm = std::mem::replace(&mut self.pending, rest);
        self.sent |= !pcm.is_empty();
        send(ws, Packet::audio(pcm, last)).await
    }
}

/// A session that heard nothing ends with an empty final result.
async fn end_silent(ws: &mut Socket, results: &mpsc::Sender<Output>) -> Result<(), EngineError> {
    let _ = results
        .send(Output::Result(Recognition {
            text: String::new(),
            utterances: Vec::new(),
            is_final: true,
        }))
        .await;
    close(ws).await;
    Ok(())
}

async fn send(ws: &mut Socket, packet: Packet) -> Result<(), EngineError> {
    let bytes = Message::Packet(packet).encode();
    ws.send(WsMessage::Binary(bytes.into()))
        .await
        .map_err(|e| EngineError::Connect(e.to_string()))
}

/// Says goodbye without waiting for the service's answer.
async fn close(ws: &mut Socket) {
    let frame = CloseFrame {
        code: CloseCode::Normal,
        reason: "done".into(),
    };
    let _ = tokio::time::timeout(CLOSE_WAIT, ws.close(Some(frame))).await;
}

/// The upgrade request: endpoint, credentials and resource.
fn handshake(config: &Config) -> Result<Request, EngineError> {
    let mut request = config
        .url()
        .into_client_request()
        .map_err(|_| EngineError::Invalid("base_host".to_owned()))?;
    let headers = request.headers_mut();
    let mut set = |name: &'static str, value: &str, key: &str, secret: bool| {
        let mut value =
            HeaderValue::from_str(value).map_err(|_| EngineError::Invalid(key.to_owned()))?;
        value.set_sensitive(secret);
        headers.insert(HeaderName::from_static(name), value);
        Ok::<(), EngineError>(())
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let connect_id = uuid::Uuid::new_v4().to_string();
    set("x-api-request-id", &request_id, "", false)?;
    set("x-api-connect-id", &connect_id, "", false)?;
    set(
        "x-api-resource-id",
        &config.resource_id,
        "resource_id",
        false,
    )?;
    set("x-api-sequence", "-1", "", false)?;
    match &config.auth {
        Auth::ApiKey(key) => set("x-api-key", key, "api_key", true)?,
        Auth::Legacy { app_id, access_key } => {
            // The service reads the app id under either name.
            set("x-api-app-id", app_id, "app_id", false)?;
            set("x-api-app-key", app_id, "app_id", false)?;
            set("x-api-access-key", access_key, "access_key", true)?;
        }
    }
    Ok(request)
}

/// The service's trace id for this connection, for its support.
fn logid(headers: &HeaderMap) -> String {
    headers
        .get("x-tt-logid")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn with_logid(detail: String, logid: &str) -> String {
    if logid.is_empty() {
        detail
    } else {
        format!("{detail} (logid {logid})")
    }
}

fn connect_error(e: tungstenite::Error) -> EngineError {
    match e {
        tungstenite::Error::Http(response) => {
            let body = response
                .body()
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            let body: String = body.trim().chars().take(BODY_CHARS).collect();
            let mut detail = format!("HTTP {}", response.status());
            if !body.is_empty() {
                detail = format!("{detail}: {body}");
            }
            EngineError::Connect(with_logid(detail, &logid(response.headers())))
        }
        e => EngineError::Connect(e.to_string()),
    }
}
