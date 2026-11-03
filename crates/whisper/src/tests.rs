//! Downloads against a local source, and what a managed setting needs.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anya_core::engine::{Engine, EngineError, Session, SessionParams, batch_session};
use anya_protocol::{Manifest, Text};
use ring::digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::*;

const BODY_LEN: usize = 200_000;

fn body() -> Vec<u8> {
    (0..BODY_LEN).map(|i| (i % 251) as u8).collect()
}

fn hex(bytes: &[u8]) -> String {
    digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

static TINY_SHA1: LazyLock<String> = LazyLock::new(|| hex(&body()));

/// A catalog of one model, `tiny`, whose pinned hash is `body()`'s.
static CATALOG: LazyLock<[Model; 1]> = LazyLock::new(|| {
    [Model {
        id: "tiny",
        file: "ggml-tiny.bin",
        bytes: BODY_LEN as u64,
        sha1: TINY_SHA1.as_str(),
        note: ["Test", "测试"],
    }]
});

fn catalog() -> &'static [Model] {
    CATALOG.as_slice()
}

/// How the source answers one request.
#[derive(Clone, Copy)]
enum Answer {
    /// The file, honouring a range.
    Whole,
    /// Says the whole length but closes after `n` bytes.
    Cut(usize),
    /// Other bytes than the catalog's.
    Wrong,
    /// This status and no body.
    Status(u16),
}

/// A source answering in `answers` order (the last repeats); it sends back
/// each request's `Range` header.
async fn source(answers: Vec<Answer>) -> (String, mpsc::UnboundedReceiver<Option<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut answers = answers.into_iter();
        let mut last = Answer::Whole;
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).to_lowercase();
            let range = head
                .lines()
                .find_map(|l| l.strip_prefix("range: bytes="))
                .map(|r| r.trim_end_matches('-').to_owned());
            let _ = tx.send(range.clone());
            let answer = answers.next().unwrap_or(last);
            last = answer;
            let from: usize = range.and_then(|r| r.parse().ok()).unwrap_or(0);
            let (status, data, cut) = match answer {
                Answer::Whole | Answer::Cut(_) if from > 0 => (206, body()[from..].to_vec(), None),
                Answer::Whole => (200, body(), None),
                Answer::Cut(n) => (200, body(), Some(n)),
                Answer::Wrong => (200, vec![7; BODY_LEN], None),
                Answer::Status(s) => (s, Vec::new(), None),
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&data[..cut.unwrap_or(data.len())]).await;
            let _ = stream.shutdown().await;
        }
    });
    (base, rx)
}

fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// Waits until model `tiny` is no longer downloading, and returns its state.
async fn settled(local: &Local) -> ModelState {
    for _ in 0..500 {
        let state = local.status(&BTreeMap::new()).models[0].state.clone();
        if !matches!(
            state,
            ModelState::Downloading { .. } | ModelState::Verifying
        ) {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the download did not settle");
}

#[tokio::test]
async fn a_broken_download_resumes_from_what_is_on_disk_and_ends_checked() {
    let dir = tempfile::tempdir().expect("dir");
    let local = Local::with_catalog(dir.path().to_owned(), catalog());
    let (base, mut ranges) = source(vec![Answer::Cut(80_000), Answer::Whole]).await;
    let source = settings(&[(SOURCE, &format!("{base}/"))]);

    local.download("tiny", &source).expect("a known model");
    let state = settled(&local).await;
    assert!(
        matches!(
            state,
            ModelState::Failed {
                error: DownloadError::Connect { .. },
                have: 80_000
            }
        ),
        "{state:?}"
    );
    assert_eq!(ranges.recv().await, Some(None));

    local.download("tiny", &source).expect("a known model");
    assert_eq!(
        settled(&local).await,
        ModelState::Ready {
            bytes: BODY_LEN as u64
        }
    );
    assert_eq!(ranges.recv().await, Some(Some("80000".to_owned())));
    assert_eq!(
        std::fs::read(dir.path().join("ggml-tiny.bin")).expect("the model"),
        body()
    );
    assert!(!dir.path().join("ggml-tiny.bin.part").exists());
    assert_eq!(local.status(&BTreeMap::new()).used, BODY_LEN as u64);

    local.delete("tiny").await.expect("deleted");
    assert_eq!(settled(&local).await, ModelState::Absent);
}

#[tokio::test]
async fn a_file_that_is_not_the_pinned_model_is_deleted_and_never_ready() {
    let dir = tempfile::tempdir().expect("dir");
    let local = Local::with_catalog(dir.path().to_owned(), catalog());
    let (base, _) = source(vec![Answer::Wrong]).await;
    local
        .download("tiny", &settings(&[(SOURCE, &base)]))
        .expect("a known model");
    assert_eq!(
        settled(&local).await,
        ModelState::Failed {
            error: DownloadError::Mismatch,
            have: 0
        }
    );
    assert!(std::fs::read_dir(dir.path()).expect("dir").next().is_none());
}

#[tokio::test]
async fn a_source_error_or_a_bad_source_fails_with_its_reason() {
    let dir = tempfile::tempdir().expect("dir");
    let local = Local::with_catalog(dir.path().to_owned(), catalog());
    let (base, _) = source(vec![Answer::Status(404)]).await;
    local
        .download("tiny", &settings(&[(SOURCE, &base)]))
        .expect("a known model");
    assert_eq!(
        settled(&local).await,
        ModelState::Failed {
            error: DownloadError::Http(404),
            have: 0
        }
    );

    local
        .download("tiny", &settings(&[(SOURCE, "ftp://mirror.example")]))
        .expect("a known model");
    assert!(matches!(
        settled(&local).await,
        ModelState::Failed {
            error: DownloadError::InvalidSource(_),
            ..
        }
    ));
    assert_eq!(
        local.download("huge", &BTreeMap::new()),
        Err(UnknownModel("huge".to_owned()))
    );
}

/// An engine that needs nothing, for what the wrapper adds.
struct Plain(Manifest);

impl Engine for Plain {
    fn manifest(&self) -> &Manifest {
        &self.0
    }

    fn open(&self, _: SessionParams) -> Session {
        batch_session(|_| async { Ok(String::new()) })
    }
}

#[test]
fn a_managed_server_needs_a_model_on_disk_and_a_program() {
    let dir = tempfile::tempdir().expect("dir");
    let local = Local::with_catalog(dir.path().to_owned(), catalog());
    let engine = local.wrap(Arc::new(Plain(Manifest {
        id: "plain".to_owned(),
        title: Text::default(),
        blurb: Text::default(),
        docs: None,
        streaming: false,
        fields: Vec::new(),
    })));
    let keys: Vec<_> = engine
        .manifest()
        .fields
        .iter()
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(keys, [MODE, MODEL, PROGRAM, IDLE, SOURCE]);

    let program = dir.path().join("whisper-server");
    std::fs::write(&program, "").expect("program");
    let program = program.to_string_lossy().into_owned();
    let file = dir.path().join("mine.bin");
    std::fs::write(&file, "").expect("model");
    let file = file.to_string_lossy().into_owned();
    let check = |pairs: &[(&str, &str)]| engine.check(&settings(pairs));

    assert_eq!(check(&[]), Ok(()), "external needs nothing more");
    assert_eq!(
        check(&[(MODE, "elsewhere")]),
        Err(EngineError::Invalid(MODE.to_owned()))
    );
    assert_eq!(
        check(&[(MODE, MANAGED), (PROGRAM, &program)]),
        Err(EngineError::NotConfigured(MODEL.to_owned()))
    );
    assert_eq!(
        check(&[(MODE, MANAGED), (MODEL, "tiny"), (PROGRAM, &program)]),
        Err(EngineError::NotConfigured(MODEL.to_owned())),
        "a model not downloaded yet"
    );
    assert_eq!(
        check(&[
            (MODE, MANAGED),
            (MODEL, "/no/such.bin"),
            (PROGRAM, &program)
        ]),
        Err(EngineError::Invalid(MODEL.to_owned()))
    );
    assert_eq!(
        check(&[
            (MODE, MANAGED),
            (MODEL, &file),
            (PROGRAM, "/no/whisper-server")
        ]),
        Err(EngineError::Invalid(PROGRAM.to_owned()))
    );
    assert_eq!(
        check(&[
            (MODE, MANAGED),
            (MODEL, &file),
            (PROGRAM, &program),
            (IDLE, "soon")
        ]),
        Err(EngineError::Invalid(IDLE.to_owned()))
    );
    assert_eq!(
        check(&[(MODE, MANAGED), (MODEL, &file), (PROGRAM, &program)]),
        Ok(())
    );
}
