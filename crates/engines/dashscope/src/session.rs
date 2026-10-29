//! One recognition task over one WebSocket.

use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{EngineError, Input, Output, Session, SessionParams};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::config::{Config, language_hint};
use crate::resample::Halve;
use crate::transcript::Transcript;
use crate::wire::{self, Event};

/// How long connecting and the service's `task-started` may take together.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a close handshake may hold the session's task.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Audio per binary frame at most: 100 ms at 16 kHz, as the service suggests.
const FRAME_BYTES: usize = 3200;
/// Chunks kept while connecting: ten seconds, more than [`START_TIMEOUT`] needs
/// before the session gives up, since connecting drains them as they come.
const INPUT_CAPACITY: usize = 256;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Opens a session; connecting runs on a spawned task.
pub(crate) fn open(params: SessionParams) -> Session {
    let (input, audio) = mpsc::channel(INPUT_CAPACITY);
    let (results, output) = mpsc::channel(64);
    tokio::spawn(async move {
        if let Err(e) = run(params, audio, &results).await {
            tracing::debug!(error = %e, "DashScope session failed");
            let _ = results.send(Output::Error(e)).await;
        }
    });
    Session { input, output }
}

/// The whole session. `Ok` once it ended or was abandoned; the final result,
/// if any, has been sent.
async fn run(
    params: SessionParams,
    mut audio: mpsc::Receiver<Input>,
    results: &mpsc::Sender<Output>,
) -> Result<(), EngineError> {
    let cfg = Config::from_settings(&params.settings)?;
    let hint = params
        .language
        .as_deref()
        .or(cfg.language.as_deref())
        .and_then(language_hint);
    let task_id = uuid::Uuid::new_v4().to_string();
    tracing::debug!(url = %cfg.url, model = %cfg.model, rate = cfg.sample_rate, "connecting to DashScope");

    // Audio that arrives while connecting is kept, so the caller never waits.
    let mut buffered = Vec::new();
    let mut finishing = false;
    let mut ws = {
        let connect = tokio::time::timeout(START_TIMEOUT, start(&cfg, &task_id, hint.as_deref()));
        tokio::pin!(connect);
        loop {
            tokio::select! {
                started = &mut connect => match started {
                    Ok(ws) => break ws?,
                    Err(_) => {
                        return Err(EngineError::Connect(format!(
                            "no task-started within {} s",
                            START_TIMEOUT.as_secs()
                        )));
                    }
                },
                input = audio.recv(), if !finishing => match input {
                    Some(Input::Audio(chunk)) => buffered.push(chunk),
                    Some(Input::Finish) => finishing = true,
                    None => return Ok(()),
                },
                () = results.closed() => return Ok(()),
            }
        }
    };
    if results.send(Output::Ready).await.is_err() {
        close(&mut ws).await;
        return Ok(());
    }
    let streamed = stream(
        &mut ws,
        &task_id,
        Uplink::new(cfg.sample_rate),
        buffered,
        finishing,
        audio,
        results,
    )
    .await;
    close(&mut ws).await;
    streamed
}

/// Dials, sends `run-task` and waits for `task-started`.
async fn start(cfg: &Config, task_id: &str, hint: Option<&str>) -> Result<Ws, EngineError> {
    let mut request = cfg
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| EngineError::Connect(e.to_string()))?;
    let auth = HeaderValue::from_str(&format!("Bearer {}", cfg.api_key))
        .map_err(|_| EngineError::Connect("the API key is not a valid header value".to_owned()))?;
    request.headers_mut().insert(AUTHORIZATION, auth);
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(dial_error)?;
    ws.send(Message::text(wire::run_task(
        task_id,
        &cfg.model,
        cfg.sample_rate,
        hint,
    )))
    .await
    .map_err(sending)?;
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => match wire::parse(&text) {
                Some(Event::Started) => return Ok(ws),
                Some(Event::Failed(detail)) => {
                    close(&mut ws).await;
                    return Err(EngineError::Upstream(detail));
                }
                Some(Event::Sentence { .. } | Event::Finished) | None => {}
            },
            Some(Ok(Message::Close(frame))) => return Err(closed(frame)),
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(EngineError::Connect(e.to_string())),
            None => {
                return Err(EngineError::Connect(
                    "the connection closed before task-started".to_owned(),
                ));
            }
        }
    }
}

/// Sends audio and reads results until `task-finished`, a failure, or the
/// caller abandoning the session.
async fn stream(
    ws: &mut Ws,
    task_id: &str,
    mut uplink: Uplink,
    buffered: Vec<Chunk>,
    mut finishing: bool,
    mut audio: mpsc::Receiver<Input>,
    results: &mpsc::Sender<Output>,
) -> Result<(), EngineError> {
    for Chunk(pcm) in buffered {
        uplink.send(ws, pcm).await?;
    }
    if finishing {
        ws.send(Message::text(wire::finish_task(task_id)))
            .await
            .map_err(sending)?;
    }
    let mut transcript = Transcript::default();
    loop {
        tokio::select! {
            input = audio.recv(), if !finishing => match input {
                Some(Input::Audio(Chunk(pcm))) => uplink.send(ws, pcm).await?,
                Some(Input::Finish) => {
                    finishing = true;
                    ws.send(Message::text(wire::finish_task(task_id)))
                        .await
                        .map_err(sending)?;
                }
                None => return Ok(()),
            },
            message = ws.next() => match message {
                Some(Ok(Message::Text(text))) => match wire::parse(&text) {
                    Some(Event::Sentence { text, end }) => {
                        if let Some(r) = transcript.update(&text, end)
                            && results.send(Output::Result(r)).await.is_err()
                        {
                            return Ok(());
                        }
                    }
                    Some(Event::Finished) => {
                        let _ = results.send(Output::Result(transcript.finish())).await;
                        return Ok(());
                    }
                    Some(Event::Failed(detail)) => return Err(EngineError::Upstream(detail)),
                    Some(Event::Started) | None => {}
                },
                Some(Ok(Message::Close(frame))) => return Err(closed(frame)),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(EngineError::Connect(e.to_string())),
                None => {
                    return Err(EngineError::Connect(
                        "the connection closed before task-finished".to_owned(),
                    ));
                }
            },
            () = results.closed() => return Ok(()),
        }
    }
}

/// Audio on its way up: halved for 8 kHz models, split into frames.
struct Uplink {
    halve: Option<Halve>,
}

impl Uplink {
    fn new(sample_rate: u32) -> Uplink {
        Uplink {
            halve: (sample_rate == 8000).then(Halve::default),
        }
    }

    async fn send(&mut self, ws: &mut Ws, pcm: Vec<u8>) -> Result<(), EngineError> {
        let pcm = match &mut self.halve {
            Some(halve) => halve.push(&pcm),
            None => pcm,
        };
        if pcm.len() <= FRAME_BYTES {
            if !pcm.is_empty() {
                ws.send(Message::binary(pcm)).await.map_err(sending)?;
            }
            return Ok(());
        }
        for frame in pcm.chunks(FRAME_BYTES) {
            ws.send(Message::binary(frame.to_vec()))
                .await
                .map_err(sending)?;
        }
        Ok(())
    }
}

/// Starts the close handshake without waiting long for the service's reply.
async fn close(ws: &mut Ws) {
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close(None)).await;
}

fn dial_error(e: WsError) -> EngineError {
    match e {
        WsError::Http(response) => {
            let body = response
                .body()
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            let body = body.trim();
            EngineError::Upstream(if body.is_empty() {
                format!("HTTP {}", response.status())
            } else {
                format!("HTTP {}: {body}", response.status())
            })
        }
        e => EngineError::Connect(e.to_string()),
    }
}

fn sending(e: WsError) -> EngineError {
    EngineError::Connect(format!("sending to DashScope: {e}"))
}

fn closed(frame: Option<CloseFrame>) -> EngineError {
    EngineError::Upstream(match frame {
        Some(f) if !f.reason.is_empty() => format!(
            "the service closed the connection ({}): {}",
            u16::from(f.code),
            f.reason.as_str()
        ),
        Some(f) => format!("the service closed the connection ({})", u16::from(f.code)),
        None => "the service closed the connection".to_owned(),
    })
}
