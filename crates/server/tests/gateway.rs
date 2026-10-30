//! The gateway over real sockets: pairing, tokens, scopes and remote
//! dictation on loopback, and the same behind TLS on the LAN listener.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_core::audio::CHUNK_BYTES;
use anya_core::engine::{Choice, Engine, EngineError, Input, Output, Session, SessionParams};
use anya_core::hub::Hub;
use anya_protocol::{Frame, Manifest, Scope, Source, SourceState, contract};
use anya_server::{Clients, Gateway, Server, TlsIdentity, parse_address};
use common::*;
use ring::digest::{SHA256, digest};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};

/// An engine that is ready at once. A streaming one reports a partial
/// result per chunk; a batch one answers only after `finish`.
struct Script {
    manifest: Manifest,
    bytes: Arc<AtomicUsize>,
    params: Arc<Mutex<Vec<SessionParams>>>,
}

impl Script {
    fn new(streaming: bool) -> Script {
        Script {
            manifest: Manifest {
                id: "script".to_owned(),
                title: "Script".into(),
                blurb: "".into(),
                docs: None,
                streaming,
                fields: Vec::new(),
            },
            bytes: Arc::default(),
            params: Arc::default(),
        }
    }
}

impl Engine for Script {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, params: SessionParams) -> Session {
        self.params.lock().expect("params").push(params);
        let (input, mut inputs) = mpsc::channel(64);
        let (outputs, output) = mpsc::channel(64);
        let streaming = self.manifest.streaming;
        let bytes = Arc::clone(&self.bytes);
        tokio::spawn(async move {
            let _ = outputs.send(Output::Ready).await;
            let mut chunks = 0;
            while let Some(message) = inputs.recv().await {
                match message {
                    Input::Audio(chunk) => {
                        bytes.fetch_add(chunk.0.len(), Ordering::SeqCst);
                        chunks += 1;
                        if streaming {
                            let text = format!("heard {chunks}");
                            let _ = outputs
                                .send(Output::Result(rec(&[(&text, false)], false)))
                                .await;
                        }
                    }
                    Input::Finish => {
                        let _ = outputs
                            .send(Output::Result(rec(&[("done", true)], true)))
                            .await;
                        return;
                    }
                }
            }
        });
        Session { input, output }
    }
}

struct Rig {
    hub: Hub,
    clients: Clients,
    server: Server,
    engine: Arc<Script>,
    _dir: TempDir,
}

fn gateway(clients: &Clients, choice: Choice) -> Gateway {
    Gateway {
        choice,
        clients: clients.clone(),
        final_timeout: Duration::from_secs(5),
    }
}

fn script_choice(engine: &Arc<Script>) -> Choice {
    let engine = Arc::clone(engine);
    Arc::new(move || {
        let shared: Arc<dyn Engine> = engine.clone();
        Ok((
            shared,
            SessionParams {
                language: Some("en".to_owned()),
                hotwords: vec!["base".to_owned()],
                ..SessionParams::default()
            },
        ))
    })
}

async fn rig(streaming: bool) -> Rig {
    let dir = TempDir::new("gw");
    let clients = Clients::open(dir.path().join("clients.json"));
    rig_with(streaming, dir, clients).await
}

async fn rig_with(streaming: bool, dir: TempDir, clients: Clients) -> Rig {
    let engine = Arc::new(Script::new(streaming));
    let hub = hub();
    let gw = gateway(&clients, script_choice(&engine));
    let server = Server::start(
        hub.clone(),
        parse_address("127.0.0.1:0").expect("address"),
        None,
        Some(gw),
    )
    .await
    .expect("start");
    Rig {
        hub,
        clients,
        server,
        engine,
        _dir: dir,
    }
}

/// What the client sees of a handshake.
struct Dialed {
    client: Client,
    protocol: Option<String>,
}

/// Connects to a `ws://` or `wss://` URL on loopback; `pin` is the
/// certificate fingerprint a `wss://` server must have.
async fn dial(
    url: &str,
    bearer: Option<&str>,
    protocols: &[&str],
    pin: Option<&str>,
) -> Result<Dialed, tungstenite::Error> {
    let (scheme, rest) = url.split_once("://").expect("a url");
    let port = rest
        .strip_suffix("/asr")
        .and_then(|r| r.rsplit(':').next())
        .expect("a port");
    let addr = format!("127.0.0.1:{port}");
    let tcp = TcpStream::connect(&addr).await?;
    let stream: Box<dyn Io> = match pin {
        Some(pin) => {
            let connector = tokio_rustls::TlsConnector::from(pinned(pin));
            let name = ServerName::try_from("localhost").expect("a name");
            Box::new(connector.connect(name, tcp).await?)
        }
        None => Box::new(tcp),
    };
    let mut request = format!("{scheme}://{addr}/asr").into_client_request()?;
    if let Some(token) = bearer {
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("a header"),
        );
    }
    if !protocols.is_empty() {
        request.headers_mut().insert(
            "sec-websocket-protocol",
            protocols.join(", ").parse().expect("a header"),
        );
    }
    let (client, response) = tokio_tungstenite::client_async(request, stream).await?;
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .map(|v| v.to_str().expect("ascii").to_owned());
    Ok(Dialed { client, protocol })
}

/// Accepts the one certificate whose fingerprint is `pin`, whatever its name.
#[derive(Debug)]
struct Pinned(String);

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity.as_ref()) == self.0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("not the pinned certificate".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn fingerprint(der: &[u8]) -> String {
    let pairs: Vec<String> = digest(&SHA256, der)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    pairs.join(":")
}

fn pinned(pin: &str) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned(pin.to_owned())))
        .with_no_client_auth();
    Arc::new(config)
}

async fn send(client: &mut Client, message: Value) {
    client
        .send(Message::text(message.to_string()))
        .await
        .expect("send");
}

async fn send_audio(client: &mut Client, bytes: usize) {
    client
        .send(Message::binary(vec![0x10u8; bytes]))
        .await
        .expect("send audio");
}

async fn next_json(client: &mut Client) -> Value {
    serde_json::from_str(&next_text(client).await).expect("JSON")
}

fn kind(value: &Value) -> &str {
    value["type"].as_str().expect("a type")
}

/// Reads until a message of type `wanted`, returning everything read.
async fn until_kind(client: &mut Client, wanted: &str) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let value = next_json(client).await;
        let done = kind(&value) == wanted;
        seen.push(value);
        if done {
            return seen;
        }
    }
}

/// A fresh connection without a token, past its `hello`.
async fn anonymous(url: &str, pin: Option<&str>) -> Client {
    let mut client = dial(url, None, &["anya.v0"], pin)
        .await
        .expect("dial")
        .client;
    assert_eq!(kind(&next_json(&mut client).await), "hello");
    client
}

/// Pairs a new client; returns its id and token.
async fn pair(rig_url: &str, clients: &Clients, name: &str, pin: Option<&str>) -> (String, String) {
    let code = clients.start_pairing().code;
    let mut client = anonymous(rig_url, pin).await;
    send(
        &mut client,
        json!({"type": "pair", "code": code, "name": name}),
    )
    .await;
    let paired = next_json(&mut client).await;
    assert_eq!(kind(&paired), "paired", "{paired}");
    (
        paired["client"].as_str().expect("id").to_owned(),
        paired["token"].as_str().expect("token").to_owned(),
    )
}

/// A connection authenticated by `token`, past its `hello`.
async fn as_client(url: &str, token: &str, pin: Option<&str>) -> Client {
    let mut client = dial(url, Some(token), &["anya.v0"], pin)
        .await
        .expect("dial")
        .client;
    assert_eq!(kind(&next_json(&mut client).await), "hello");
    client
}

fn start_message(id: &str) -> Value {
    json!({"type": "dictation_start", "request_id": id})
}

/// Starts a dictation on `client` and reads through `session_start`.
async fn start_dictation(client: &mut Client, id: &str) -> String {
    send(client, start_message(id)).await;
    let accepted = next_json(client).await;
    assert_eq!(kind(&accepted), "dictation_accepted", "{accepted}");
    assert_eq!(accepted["request_id"], id);
    let session = accepted["session"].as_str().expect("session").to_owned();
    let started = until_kind(client, "session_start").await;
    assert_eq!(
        started.len(),
        1,
        "nothing between accepted and start: {started:?}"
    );
    session
}

/// Every message through `session_end`, as frames (replies read as unknown).
fn as_frames(messages: &[Value]) -> Vec<Frame> {
    let hello = Frame::Hello {
        name: "anya".to_owned(),
        version: String::new(),
        protocol: 0,
    };
    std::iter::once(hello)
        .chain(
            messages
                .iter()
                .map(|m| serde_json::from_value(m.clone()).expect("a frame")),
        )
        .collect()
}

#[tokio::test]
async fn a_code_pairs_once_and_the_client_is_listed() {
    let rig = rig(true).await;
    let code = rig.clients.start_pairing().code;
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|b| b.is_ascii_digit()));

    let mut client = anonymous(&rig.server.url(), None).await;
    send(
        &mut client,
        json!({"type": "pair", "code": code, "name": "iPhone"}),
    )
    .await;
    let paired = next_json(&mut client).await;
    assert_eq!(kind(&paired), "paired");
    assert_eq!(paired["scopes"], json!(["dictate"]));
    let id = paired["client"].as_str().expect("id");
    assert!(id.starts_with("c-") && id.len() == 10, "{id}");
    assert_eq!(paired["token"].as_str().expect("token").len(), 64);

    let listed = rig.clients.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].name, "iPhone");
    assert_eq!(listed[0].scopes, vec![Scope::Dictate]);
    assert_eq!(
        listed[0].connections, 1,
        "the pairing connection is the client"
    );

    // The same connection now acts as that client.
    send(&mut client, start_message("r1")).await;
    assert_eq!(kind(&next_json(&mut client).await), "dictation_accepted");

    // A code works once.
    let mut other = anonymous(&rig.server.url(), None).await;
    send(
        &mut other,
        json!({"type": "pair", "code": code, "name": "again"}),
    )
    .await;
    let refused = next_json(&mut other).await;
    assert_eq!(refused["type"], "refused");
    assert_eq!(refused["reason"], "pairing_failed");
    assert_eq!(rig.clients.list().len(), 1);
}

#[tokio::test]
async fn five_wrong_codes_void_the_code() {
    let rig = rig(true).await;
    let code = rig.clients.start_pairing().code;
    let wrong = if code == "000000" { "000001" } else { "000000" };
    let mut client = anonymous(&rig.server.url(), None).await;
    let mut reasons = Vec::new();
    for _ in 0..5 {
        send(
            &mut client,
            json!({"type": "pair", "code": wrong, "name": "x"}),
        )
        .await;
        reasons.push(next_json(&mut client).await["reason"].clone());
    }
    assert_eq!(
        reasons,
        [
            "pairing_failed",
            "pairing_failed",
            "pairing_failed",
            "pairing_failed",
            "too_many_attempts"
        ]
    );
    // Even the right code is void now.
    send(
        &mut client,
        json!({"type": "pair", "code": code, "name": "x"}),
    )
    .await;
    assert_eq!(next_json(&mut client).await["reason"], "too_many_attempts");
    assert!(rig.clients.list().is_empty());

    // A fresh code starts over.
    let fresh = rig.clients.start_pairing().code;
    send(
        &mut client,
        json!({"type": "pair", "code": fresh, "name": "x"}),
    )
    .await;
    assert_eq!(kind(&next_json(&mut client).await), "paired");
}

#[tokio::test]
async fn an_expired_code_does_not_pair() {
    let dir = TempDir::new("gwexp");
    let clients =
        Clients::open_with_window(dir.path().join("clients.json"), Duration::from_millis(50));
    let rig = rig_with(true, dir, clients).await;
    let code = rig.clients.start_pairing().code;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut client = anonymous(&rig.server.url(), None).await;
    send(
        &mut client,
        json!({"type": "pair", "code": code, "name": "late"}),
    )
    .await;
    assert_eq!(next_json(&mut client).await["reason"], "pairing_failed");
    assert!(rig.clients.list().is_empty());
}

#[tokio::test]
async fn a_new_code_replaces_the_previous_one() {
    let rig = rig(true).await;
    let first = rig.clients.start_pairing().code;
    let second = loop {
        let code = rig.clients.start_pairing().code;
        if code != first {
            break code;
        }
    };
    let mut client = anonymous(&rig.server.url(), None).await;
    send(
        &mut client,
        json!({"type": "pair", "code": first, "name": "x"}),
    )
    .await;
    assert_eq!(next_json(&mut client).await["reason"], "pairing_failed");
    send(
        &mut client,
        json!({"type": "pair", "code": second, "name": "x"}),
    )
    .await;
    assert_eq!(kind(&next_json(&mut client).await), "paired");
}

#[tokio::test]
async fn paired_clients_survive_a_restart_with_their_token_hashed() {
    let rig = rig(true).await;
    let (id, token) = pair(&rig.server.url(), &rig.clients, "Laptop", None).await;
    rig.clients
        .set_scopes(&id, vec![Scope::Dictate, Scope::SubscribeContinuous])
        .expect("scopes");
    let path = rig._dir.path().join("clients.json");
    let text = std::fs::read_to_string(&path).expect("clients file");
    assert!(!text.contains(&token), "the token itself is never stored");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let reopened = Clients::open(path);
    let listed = reopened.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Laptop");
    assert_eq!(
        listed[0].scopes,
        vec![Scope::Dictate, Scope::SubscribeContinuous]
    );
    assert!(listed[0].last_seen.is_some());
}

#[tokio::test]
async fn a_token_works_as_a_bearer_header_and_as_a_subprotocol() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (_, token) = pair(&url, &rig.clients, "phone", None).await;

    let mut bearer = dial(&url, Some(&token), &["anya.v0"], None)
        .await
        .expect("bearer");
    assert_eq!(bearer.protocol.as_deref(), Some("anya.v0"));
    assert_eq!(kind(&next_json(&mut bearer.client).await), "hello");
    send(&mut bearer.client, start_message("b")).await;
    assert_eq!(
        kind(&next_json(&mut bearer.client).await),
        "dictation_accepted"
    );

    let offered = format!("anya.token.{token}");
    let mut browser = dial(&url, None, &["anya.v0", &offered], None)
        .await
        .expect("subprotocol");
    assert_eq!(
        browser.protocol.as_deref(),
        Some("anya.v0"),
        "the token is never echoed"
    );
    assert_eq!(kind(&next_json(&mut browser.client).await), "hello");
    send(&mut browser.client, start_message("s")).await;
    assert_eq!(
        kind(&next_json(&mut browser.client).await),
        "dictation_accepted"
    );
}

#[tokio::test]
async fn an_unknown_token_is_refused_at_the_handshake() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let status = |result: Result<Dialed, tungstenite::Error>| match result {
        Err(tungstenite::Error::Http(response)) => Some(response.status().as_u16()),
        _ => None,
    };
    assert_eq!(
        status(dial(&url, Some("nope"), &["anya.v0"], None).await),
        Some(401)
    );
    assert_eq!(
        status(dial(&url, None, &["anya.v0", "anya.token.nope"], None).await),
        Some(401)
    );
}

#[tokio::test]
async fn without_a_token_loopback_gets_continuous_only_and_may_not_dictate() {
    let rig = rig(true).await;
    rig.hub
        .set_source(Source::Continuous, SourceState::Listening, None);
    rig.hub
        .set_source(Source::Dictation, SourceState::Listening, None);
    let mut client = dial(&rig.server.url(), None, &["anya.v0"], None)
        .await
        .expect("dial")
        .client;
    assert_eq!(kind(&next_json(&mut client).await), "hello");
    let source = next_json(&mut client).await;
    assert_eq!(
        (kind(&source), source["source"].as_str()),
        ("source", Some("continuous"))
    );

    send(&mut client, start_message("r1")).await;
    let refused = next_json(&mut client).await;
    assert_eq!(refused["type"], "refused");
    assert_eq!(refused["reason"], "unauthorized");
    assert_eq!(refused["request_id"], "r1");
    send_audio(&mut client, CHUNK_BYTES).await;
    assert_eq!(next_json(&mut client).await["reason"], "unauthorized");
    send(
        &mut client,
        json!({"type": "dictation_cancel", "session": "x"}),
    )
    .await;
    assert_eq!(next_json(&mut client).await["reason"], "unauthorized");
}

#[tokio::test]
async fn a_scope_change_applies_to_an_open_connection() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (id, token) = pair(&url, &rig.clients, "tablet", None).await;
    rig.hub
        .set_source(Source::Continuous, SourceState::Listening, None);
    let mut client = as_client(&url, &token, None).await;

    // Dictate only: no source frames, and the refusal below is the next message.
    send(&mut client, json!("not a message")).await;
    assert_eq!(next_json(&mut client).await["reason"], "bad_request");

    rig.clients
        .set_scopes(&id, vec![Scope::Dictate, Scope::SubscribeContinuous])
        .expect("grant");
    let source = next_json(&mut client).await;
    assert_eq!(
        (kind(&source), source["state"].as_str()),
        ("source", Some("listening"))
    );
    rig.hub
        .set_source(Source::Continuous, SourceState::Idle, None);
    assert_eq!(next_json(&mut client).await["state"], "idle");

    rig.clients
        .set_scopes(&id, vec![Scope::Dictate])
        .expect("revoke");
    // The connection applies the change on its own task.
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.hub
        .set_source(Source::Continuous, SourceState::Listening, None);
    send(&mut client, json!("marker")).await;
    assert_eq!(
        next_json(&mut client).await["type"],
        "refused",
        "nothing of the revoked source arrived"
    );

    rig.clients.set_scopes(&id, vec![]).expect("revoke all");
    // The change is applied before the next message is read.
    eventually(|| rig.clients.list()[0].scopes.is_empty()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(&mut client, start_message("r")).await;
    assert_eq!(next_json(&mut client).await["reason"], "forbidden");
}

#[tokio::test]
async fn removing_a_client_closes_its_connections_and_voids_its_token() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (id, token) = pair(&url, &rig.clients, "old phone", None).await;
    let mut first = as_client(&url, &token, None).await;
    let mut second = as_client(&url, &token, None).await;
    eventually(|| rig.clients.list()[0].connections == 2).await;

    rig.clients.remove(&id).expect("remove");
    for client in [&mut first, &mut second] {
        let (_, close) = until_close(client).await;
        let close = close.expect("a close frame");
        assert_eq!(close.code, CloseCode::Policy);
        assert_eq!(close.reason.as_str(), "client removed");
    }
    assert!(rig.clients.list().is_empty());
    assert!(matches!(
        dial(&url, Some(&token), &["anya.v0"], None).await,
        Err(tungstenite::Error::Http(r)) if r.status().as_u16() == 401
    ));
    assert!(rig.clients.remove(&id).is_err(), "already gone");
}

#[tokio::test]
async fn a_streaming_dictation_shows_partials_before_stop() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (_, token) = pair(&url, &rig.clients, "phone", None).await;
    let mut client = as_client(&url, &token, None).await;

    send(
        &mut client,
        json!({"type": "dictation_start", "request_id": "r1", "language": "fr", "hotwords": ["Anya"]}),
    )
    .await;
    let mut seen = Vec::new();
    let accepted = next_json(&mut client).await;
    assert_eq!(accepted["type"], "dictation_accepted");
    assert_eq!(accepted["streaming"], true);
    let session = accepted["session"].as_str().expect("session").to_owned();
    seen.push(accepted);
    seen.extend(until_kind(&mut client, "session_start").await);

    // Two chunks, the second split across messages at an odd byte.
    send_audio(&mut client, CHUNK_BYTES + 1).await;
    send_audio(&mut client, CHUNK_BYTES - 1).await;
    let partials = until_kind(&mut client, "result").await;
    seen.extend(partials);
    assert_eq!(seen.last().expect("last")["final"], false);
    eventually(|| rig.engine.bytes.load(Ordering::SeqCst) == 2 * CHUNK_BYTES).await;

    send(
        &mut client,
        json!({"type": "dictation_stop", "session": session}),
    )
    .await;
    seen.extend(until_kind(&mut client, "session_end").await);

    let types: Vec<&str> = seen.iter().map(kind).collect();
    assert_eq!(
        &types[..2],
        ["dictation_accepted", "session_start"],
        "{types:?}"
    );
    let finals: Vec<&Value> = seen.iter().filter(|m| m["final"] == true).collect();
    assert_eq!(finals.len(), 1);
    assert_eq!(finals[0]["text"], "done");
    assert_eq!(seen.last().expect("last")["reason"], "final");
    contract::check(&as_frames(&seen)).expect("the promises hold on the client's stream");

    let params = rig.engine.params.lock().expect("params");
    assert_eq!(params[0].language.as_deref(), Some("fr"));
    assert_eq!(params[0].hotwords, ["base", "Anya"]);
}

#[tokio::test]
async fn accepted_precedes_session_start_when_the_engine_is_ready_at_once() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (_, token) = pair(&url, &rig.clients, "phone", None).await;
    for round in 0..20 {
        let mut client = as_client(&url, &token, None).await;
        send(&mut client, start_message("r")).await;
        let first = next_json(&mut client).await;
        assert_eq!(kind(&first), "dictation_accepted", "round {round}: {first}");
        let second = next_json(&mut client).await;
        assert_eq!(kind(&second), "session_start", "round {round}");
    }
}

#[tokio::test]
async fn a_batch_dictation_has_no_result_until_stop() {
    let rig = rig(false).await;
    let url = rig.server.url();
    let (_, token) = pair(&url, &rig.clients, "phone", None).await;
    let mut client = as_client(&url, &token, None).await;

    send(&mut client, start_message("r1")).await;
    let accepted = next_json(&mut client).await;
    assert_eq!(accepted["streaming"], false);
    let session = accepted["session"].as_str().expect("session").to_owned();
    let mut seen = vec![accepted];
    let started = next_json(&mut client).await;
    assert_eq!(
        (kind(&started), started["streaming"].clone()),
        ("session_start", json!(false))
    );
    seen.push(started);

    // A short tail: padded with silence to a whole chunk on stop.
    send_audio(&mut client, CHUNK_BYTES).await;
    send_audio(&mut client, 100).await;
    seen.extend(until_kind(&mut client, "level").await);
    let quiet = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let value = next_json(&mut client).await;
            if kind(&value) == "result" {
                return value;
            }
            seen.push(value);
        }
    })
    .await;
    assert!(quiet.is_err(), "a batch engine reports nothing before stop");

    send(
        &mut client,
        json!({"type": "dictation_stop", "session": session}),
    )
    .await;
    let rest = until_kind(&mut client, "session_end").await;
    let results: Vec<&Value> = rest.iter().filter(|m| kind(m) == "result").collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["final"], true);
    assert_eq!(results[0]["text"], "done");
    seen.extend(rest);
    contract::check(&as_frames(&seen)).expect("the promises hold");
    assert_eq!(rig.engine.bytes.load(Ordering::SeqCst), 2 * CHUNK_BYTES);
}

#[tokio::test]
async fn a_second_recording_is_busy_and_cancel_ends_the_first() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (_, token) = pair(&url, &rig.clients, "phone", None).await;
    let mut client = as_client(&url, &token, None).await;
    let session = start_dictation(&mut client, "r1").await;

    send(&mut client, start_message("r2")).await;
    let busy = next_json(&mut client).await;
    assert_eq!(
        (busy["type"].as_str(), busy["reason"].as_str()),
        (Some("refused"), Some("busy"))
    );
    assert_eq!(busy["request_id"], "r2");

    send(
        &mut client,
        json!({"type": "dictation_cancel", "session": "nope"}),
    )
    .await;
    let unknown = next_json(&mut client).await;
    assert_eq!(unknown["reason"], "unknown_session");
    assert_eq!(unknown["session"], "nope");
    send(
        &mut client,
        json!({"type": "dictation_stop", "session": "nope"}),
    )
    .await;
    assert_eq!(next_json(&mut client).await["reason"], "unknown_session");

    send(
        &mut client,
        json!({"type": "dictation_cancel", "session": session}),
    )
    .await;
    let end = next_json(&mut client).await;
    assert_eq!(
        (kind(&end), end["reason"].as_str()),
        ("session_end", Some("cancelled"))
    );

    // The connection can record again.
    start_dictation(&mut client, "r3").await;
}

#[tokio::test]
async fn bad_requests_are_refused_and_the_connection_stays() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (_, token) = pair(&url, &rig.clients, "phone", None).await;
    let mut client = as_client(&url, &token, None).await;

    send_audio(&mut client, CHUNK_BYTES).await;
    let refused = next_json(&mut client).await;
    assert_eq!(
        (refused["type"].as_str(), refused["reason"].as_str()),
        (Some("refused"), Some("bad_request"))
    );
    client.send(Message::text("{not json")).await.expect("send");
    assert_eq!(next_json(&mut client).await["reason"], "bad_request");
    send(&mut client, json!({"type": "from_the_future"})).await;
    assert_eq!(next_json(&mut client).await["reason"], "bad_request");
    send(
        &mut client,
        json!({"type": "pair", "code": "000000", "name": "x"}),
    )
    .await;
    assert_eq!(
        next_json(&mut client).await["reason"],
        "bad_request",
        "already a client"
    );

    start_dictation(&mut client, "r1").await;
}

#[tokio::test]
async fn an_engine_that_is_not_configured_refuses_the_start() {
    let dir = TempDir::new("gwnc");
    let clients = Clients::open(dir.path().join("clients.json"));
    let choice: Choice = Arc::new(|| Err(EngineError::NotConfigured("api_key".to_owned())));
    let server = Server::start(
        hub(),
        parse_address("127.0.0.1:0").expect("address"),
        None,
        Some(gateway(&clients, choice)),
    )
    .await
    .expect("start");
    let url = server.url();
    let (_, token) = pair(&url, &clients, "phone", None).await;
    let mut client = as_client(&url, &token, None).await;
    send(&mut client, start_message("r1")).await;
    let refused = next_json(&mut client).await;
    assert_eq!(refused["reason"], "not_configured");
    assert_eq!(refused["detail"], "api_key");
    assert_eq!(refused["request_id"], "r1");
}

#[tokio::test]
async fn closing_the_connection_cancels_the_recording() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (_, token) = pair(&url, &rig.clients, "phone", None).await;
    let mut tap = rig.hub.subscribe().frames;
    let mut client = as_client(&url, &token, None).await;
    let session = start_dictation(&mut client, "r1").await;
    drop(client);

    let ended = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(Frame::SessionEnd {
                session: s, reason, ..
            }) = tap.recv().await
                && s.0 == session
            {
                return reason;
            }
        }
    })
    .await
    .expect("the session ended");
    assert_eq!(ended, anya_protocol::EndReason::Cancelled);
    eventually(|| rig.clients.list()[0].connections == 0).await;
}

#[tokio::test]
async fn a_client_without_the_dictate_scope_is_forbidden() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (id, token) = pair(&url, &rig.clients, "watcher", None).await;
    rig.clients
        .set_scopes(&id, vec![Scope::SubscribeContinuous])
        .expect("scopes");
    let mut client = as_client(&url, &token, None).await;
    send(&mut client, start_message("r1")).await;
    let refused = next_json(&mut client).await;
    assert_eq!(refused["reason"], "forbidden");
    assert_eq!(refused["request_id"], "r1");
}

#[tokio::test]
async fn only_a_dictation_subscriber_sees_anothers_session() {
    let rig = rig(true).await;
    let url = rig.server.url();
    let (_, dictator) = pair(&url, &rig.clients, "dictator", None).await;
    let (watcher_id, watcher) = pair(&url, &rig.clients, "watcher", None).await;
    let (plain_id, plain) = pair(&url, &rig.clients, "plain", None).await;
    rig.clients
        .set_scopes(&watcher_id, vec![Scope::SubscribeDictation])
        .expect("scopes");
    rig.clients
        .set_scopes(&plain_id, vec![Scope::SubscribeContinuous])
        .expect("scopes");
    let mut watching = as_client(&url, &watcher, None).await;
    let mut plain = as_client(&url, &plain, None).await;
    let mut client = as_client(&url, &dictator, None).await;

    let session = start_dictation(&mut client, "r1").await;
    let seen = next_json(&mut watching).await;
    assert_eq!(kind(&seen), "session_start");
    assert_eq!(seen["session"], session.as_str());
    send(
        &mut client,
        json!({"type": "dictation_cancel", "session": session}),
    )
    .await;
    assert_eq!(next_json(&mut watching).await["type"], "session_end");

    send(&mut plain, json!("marker")).await;
    assert_eq!(
        next_json(&mut plain).await["type"],
        "refused",
        "it saw no session"
    );
}

#[tokio::test]
async fn the_lan_listener_serves_a_pinned_certificate_and_demands_a_token() {
    let dir = TempDir::new("gwlan");
    let tls_dir = dir.path().join("tls");
    let identity = TlsIdentity::load_or_create(&tls_dir).expect("identity");
    let pin = identity.fingerprint();
    assert_eq!(pin.len(), 32 * 3 - 1);
    assert!(pin.split(':').all(|p| {
        p.len() == 2
            && p.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }));
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode(&tls_dir), 0o700);
    assert_eq!(mode(&tls_dir.join("key.pem")), 0o600);
    assert_eq!(mode(&tls_dir.join("cert.pem")), 0o600);
    let again = TlsIdentity::load_or_create(&tls_dir).expect("reload");
    assert_eq!(again.fingerprint(), pin, "the certificate is kept");

    let clients = Clients::open(dir.path().join("clients.json"));
    let engine = Arc::new(Script::new(true));
    let hub = hub();
    hub.set_source(Source::Continuous, SourceState::Listening, None);
    let server = Server::start_lan(hub, gateway(&clients, script_choice(&engine)), 0, &identity)
        .await
        .expect("start_lan");
    let url = server.url();
    assert!(url.starts_with("wss://") && url.ends_with("/asr"), "{url}");

    // A client that pins another certificate does not get through TLS.
    let wrong = "00:".repeat(31) + "00";
    assert!(dial(&url, None, &["anya.v0"], Some(&wrong)).await.is_err());

    // Without a token: hello, then nothing but pairing.
    let mut stranger = anonymous(&url, Some(&pin)).await;
    send(&mut stranger, start_message("r1")).await;
    let refused = next_json(&mut stranger).await;
    assert_eq!(
        refused["reason"], "unauthorized",
        "no source frame came first: {refused}"
    );
    send_audio(&mut stranger, CHUNK_BYTES).await;
    assert_eq!(next_json(&mut stranger).await["reason"], "unauthorized");

    // Pairing works over TLS, from any origin, and the token dictates.
    let (_, token) = pair(&url, &clients, "iPhone", Some(&pin)).await;
    assert!(matches!(
        dial(&url, Some("nope"), &["anya.v0"], Some(&pin)).await,
        Err(tungstenite::Error::Http(r)) if r.status().as_u16() == 401
    ));
    let mut client = as_client(&url, &token, Some(&pin)).await;
    let session = start_dictation(&mut client, "r1").await;
    send_audio(&mut client, CHUNK_BYTES).await;
    let result = until_kind(&mut client, "result").await;
    assert_eq!(result.last().expect("result")["final"], false);
    send(
        &mut client,
        json!({"type": "dictation_stop", "session": session}),
    )
    .await;
    let end = until_kind(&mut client, "session_end").await;
    assert_eq!(end.last().expect("end")["reason"], "final");

    let page = {
        let offered = format!("anya.token.{token}");
        let addr = url
            .split("://")
            .nth(1)
            .and_then(|r| r.strip_suffix("/asr"))
            .expect("addr");
        let port = addr.rsplit(':').next().expect("port");
        let tcp = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .expect("tcp");
        let connector = tokio_rustls::TlsConnector::from(pinned(&pin));
        let tls = connector
            .connect(ServerName::try_from("localhost").expect("name"), tcp)
            .await
            .expect("tls");
        let mut request = format!("wss://127.0.0.1:{port}/asr")
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("origin", "https://example.com".parse().expect("header"));
        request.headers_mut().insert(
            "sec-websocket-protocol",
            format!("anya.v0, {offered}").parse().expect("header"),
        );
        tokio_tungstenite::client_async(request, tls).await
    };
    assert!(
        page.is_ok(),
        "a browser page from any origin may present a token on the LAN"
    );
    server.stop().await;
}
