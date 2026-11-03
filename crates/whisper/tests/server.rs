//! The managed server end to end. This test binary is also the fake
//! whisper-server it starts: run as `server -m <model> --host <h> --port <p>`,
//! it answers `/health` with 503 once (loading), then 200.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anya_core::audio::Chunk;
use anya_core::engine::{
    Engine, EngineError, Input, Output, Session, SessionParams, batch_session,
};
use anya_protocol::{Manifest, Text};
use anya_whisper::{Local, MANAGED, MODE, MODEL, PROGRAM, ServerState};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("-m") {
        fake_server(&args);
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(a_server_starts_once_serves_every_session_and_stops());
    runtime.block_on(a_server_that_cannot_start_fails_the_session_with_its_words());
}

fn fake_server(args: &[String]) {
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .expect("a port");
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    let mut loading = true;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            head.push(byte[0]);
        }
        let status = if std::mem::take(&mut loading) {
            "503 Service Unavailable"
        } else {
            "200 OK"
        };
        let _ = stream.write_all(
            format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .as_bytes(),
        );
    }
}

/// An engine whose one result is the server address it was opened on.
struct Echo(Manifest);

impl Engine for Echo {
    fn manifest(&self) -> &Manifest {
        &self.0
    }

    fn open(&self, params: SessionParams) -> Session {
        let url = params.settings.get("url").cloned().unwrap_or_default();
        batch_session(move |_| async move { Ok(url) })
    }
}

fn engine(local: &Arc<Local>) -> Arc<dyn Engine> {
    local.wrap(Arc::new(Echo(Manifest {
        id: "echo".to_owned(),
        title: Text::default(),
        blurb: Text::default(),
        docs: None,
        streaming: false,
        fields: Vec::new(),
    })))
}

fn managed(model: &Path, program: &Path) -> BTreeMap<String, String> {
    [
        (MODE, MANAGED.to_owned()),
        (MODEL, model.to_string_lossy().into_owned()),
        (PROGRAM, program.to_string_lossy().into_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

/// One session with some audio: its outputs.
async fn session(engine: &dyn Engine, settings: BTreeMap<String, String>) -> Vec<Output> {
    let mut s = engine.open(SessionParams {
        settings,
        ..SessionParams::default()
    });
    let _ = s.input.send(Input::Audio(Chunk(vec![0; 64]))).await;
    let _ = s.input.send(Input::Finish).await;
    let mut out = Vec::new();
    while let Some(o) = tokio::time::timeout(Duration::from_secs(30), s.output.recv())
        .await
        .expect("the session ends in time")
    {
        out.push(o);
    }
    out
}

fn text(out: &[Output]) -> String {
    match out.last() {
        Some(Output::Result(r)) => r.text.clone(),
        other => panic!("expected a result, got {other:?}"),
    }
}

async fn a_server_starts_once_serves_every_session_and_stops() {
    let dir = tempfile::tempdir().expect("dir");
    let model = dir.path().join("mine.bin");
    std::fs::write(&model, "").expect("model");
    let program = std::env::current_exe().expect("this binary");
    let local = Local::new(dir.path().join("models"));
    let engine = engine(&local);
    assert_eq!(local.status(&BTreeMap::new()).server, ServerState::Stopped);

    let first = text(&session(engine.as_ref(), managed(&model, &program)).await);
    assert!(first.starts_with("http://127.0.0.1:"), "{first}");
    let port: u16 = first
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("a port");
    match local.status(&BTreeMap::new()).server {
        ServerState::Running {
            port: p,
            sessions: 0,
            model: m,
            ..
        } => {
            assert_eq!((p, m), (port, model.clone()));
        }
        other => panic!("expected it running, got {other:?}"),
    }
    let second = text(&session(engine.as_ref(), managed(&model, &program)).await);
    assert_eq!(second, first, "the running server is reused");

    local.stop().await;
    assert_eq!(local.status(&BTreeMap::new()).server, ServerState::Stopped);
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

async fn a_server_that_cannot_start_fails_the_session_with_its_words() {
    let dir = tempfile::tempdir().expect("dir");
    let model = dir.path().join("mine.bin");
    std::fs::write(&model, "").expect("model");
    let program = dir.path().join("whisper-server");
    std::fs::write(
        &program,
        "#!/bin/sh\necho 'error: failed to load model' >&2\nexit 3\n",
    )
    .expect("program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
    }
    let local = Local::new(dir.path().join("models"));
    let out = session(engine(&local).as_ref(), managed(&model, &program)).await;
    match out.as_slice() {
        [Output::Error(EngineError::Connect(detail))] => {
            assert!(detail.contains("failed to load model"), "{detail}");
        }
        other => panic!("expected a connect error, got {other:?}"),
    }
    assert!(matches!(
        local.status(&BTreeMap::new()).server,
        ServerState::Failed { detail } if detail.contains("failed to load model")
    ));
}
