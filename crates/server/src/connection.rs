//! One live connection: the frames it is granted, and with a gateway the
//! messages it may send.
//!
//! The connection reads frames, client messages and session ends in a
//! single task, one at a time. That is what orders a reply before the
//! frames of the session it announces: the session is registered and
//! `dictation_accepted` queued before the next frame is looked at.

use std::collections::{HashMap, HashSet};

use anya_core::hub::{BUFFER, Hub};
use anya_protocol::{
    ClientMessage, EndReason, ErrorCode, Frame, Refusal, Reply, Scope, SessionId, Source,
};
use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::audience::Ledger;
use crate::clients::Attachment;
use crate::gateway::{Dictations, Ended, Gateway, Pushed, StopError};
use crate::server::{End, Inbound, Sink, encode, write_queue};

/// What a live connection is served from.
#[derive(Clone)]
pub(crate) struct Live {
    pub(crate) hub: Hub,
    /// Present when paired clients may connect and dictate.
    pub(crate) gateway: Option<Gateway>,
    /// Whether this is the LAN listener, where nothing is granted without a token.
    pub(crate) lan: bool,
    /// Where the connections note who opened a session and who was sent it.
    pub(crate) ledger: Ledger,
}

/// Serves one connection until it ends. `client` is the client its token
/// named at the handshake.
pub(crate) async fn run<S>(
    sink: &mut Sink<S>,
    inbound: &mut Inbound<S>,
    live: Live,
    client: Option<String>,
    stop: &mut watch::Receiver<bool>,
) -> End
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let subscription = live.hub.subscribe();
    let (who, scopes) = match (client, &live.gateway) {
        (Some(id), Some(gateway)) => {
            let attached = gateway.clients.attach(&id);
            let scopes = gateway.clients.scopes_of(&id);
            match (attached, scopes) {
                (Some(a), Some(scopes)) => (Some(a), scopes),
                _ => return End::Removed,
            }
        }
        // Over the LAN a connection without a token is granted nothing.
        _ if live.lan => (None, Vec::new()),
        _ => (None, vec![Scope::SubscribeContinuous]),
    };
    let mut grant = Grant::new(scopes);
    let snapshot: Vec<Message> = subscription
        .snapshot
        .iter()
        .filter(|f| grant.admits(f))
        .filter_map(encode)
        .collect();
    // Room for the snapshot on top of the frames a subscriber may fall
    // behind (docs/protocol.md, promise 10).
    let (queue, queued) = mpsc::channel(BUFFER + snapshot.len());
    for message in snapshot {
        let _ = queue.try_send(message);
    }
    let (ended_tx, mut ended) = mpsc::unbounded_channel();
    let mut changes = live.gateway.as_ref().map(|g| g.clients.changes());
    let mut conn = Conn {
        hub: live.hub,
        gateway: live.gateway,
        seat: live.ledger.seat(),
        ledger: live.ledger,
        grant,
        queue,
        who,
        dictations: Dictations::new(ended_tx),
        frames: subscription.frames,
        refused_audio: false,
    };
    let writer = write_queue(sink, queued);
    tokio::pin!(writer);
    loop {
        let deadline = conn
            .dictations
            .deadline()
            .map(tokio::time::Instant::from_std);
        let is_client = conn.who.is_some();
        let result = tokio::select! {
            biased;
            _ = stop.wait_for(|s| *s) => return End::Stopping,
            end = &mut writer => return end,
            frame = conn.frames.recv() => match frame {
                Ok(frame) => conn.forward(frame),
                Err(RecvError::Lagged(_)) => Err(End::TooSlow),
                Err(RecvError::Closed) => Err(End::Stopping),
            },
            Some(done) = ended.recv() => conn.ended(done),
            message = inbound.next() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return End::Gone,
                Some(Ok(message)) => conn.message(message),
            },
            Ok(()) = async {
                match changes.as_mut() {
                    Some(c) if is_client => c.changed().await,
                    _ => std::future::pending().await,
                }
            } => conn.rescope(),
            () = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => conn.recording_expired(),
        };
        if let Err(end) = result {
            return end;
        }
    }
}

struct Conn {
    hub: Hub,
    gateway: Option<Gateway>,
    /// This connection in the ledger.
    seat: u64,
    ledger: Ledger,
    grant: Grant,
    queue: mpsc::Sender<Message>,
    /// The client this connection acts as; `None` before it has paired.
    who: Option<Attachment>,
    dictations: Dictations,
    frames: broadcast::Receiver<Frame>,
    /// Whether audio with no recording was already answered since the last start.
    refused_audio: bool,
}

impl Conn {
    fn send(&self, message: Message) -> Result<(), End> {
        match self.queue.try_send(message) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(End::TooSlow),
            Err(TrySendError::Closed(_)) => Err(End::Gone),
        }
    }

    fn reply(&self, reply: &Reply) -> Result<(), End> {
        match serde_json::to_string(reply) {
            Ok(json) => self.send(Message::text(json)),
            Err(e) => {
                tracing::warn!(error = %e, "a reply could not be encoded");
                Ok(())
            }
        }
    }

    fn refuse(
        &self,
        request_id: Option<String>,
        session: Option<SessionId>,
        reason: Refusal,
        detail: Option<&str>,
    ) -> Result<(), End> {
        self.reply(&Reply::Refused {
            request_id,
            session,
            reason,
            detail: detail.map(str::to_owned),
        })
    }

    /// Queues `frame` if this connection is granted it.
    fn forward(&mut self, frame: Frame) -> Result<(), End> {
        if !self.grant.admits(&frame) {
            return Ok(());
        }
        let frame = match frame {
            // The connection itself threw the session away because the
            // upload outran the engine: that is an audio error to the client.
            Frame::SessionEnd {
                session,
                reason: EndReason::Cancelled,
                ..
            } if self.dictations.fault(&session).is_some() => Frame::SessionEnd {
                reason: EndReason::Error,
                error: Some(ErrorCode::Audio),
                detail: self.dictations.fault(&session).map(str::to_owned),
                session,
            },
            frame => frame,
        };
        if let Frame::Result { session, .. } = &frame
            && !self.grant.own.contains(session)
        {
            self.ledger.read(session, self.seat);
        }
        match encode(&frame) {
            Some(message) => self.send(message),
            None => Ok(()),
        }
    }

    /// A session of this connection ended. When no frame of it was ever
    /// published (the engine failed before it was ready, or the client
    /// cancelled that early), the client still gets its start and end.
    fn ended(&mut self, ended: Ended) -> Result<(), End> {
        // The hub publishes a session's end before it reports the outcome,
        // so everything about it is already waiting here.
        loop {
            match self.frames.try_recv() {
                Ok(frame) => self.forward(frame)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(_)) => return Err(End::TooSlow),
                Err(TryRecvError::Closed) => return Err(End::Stopping),
            }
        }
        let Some(done) = self.dictations.finished(&ended.session) else {
            return Ok(());
        };
        if !self.grant.own.contains(&ended.session) {
            return Ok(());
        }
        let outcome = ended.outcome;
        let (reason, error, detail) = match done.fault {
            Some(fault) => (EndReason::Error, Some(ErrorCode::Audio), Some(fault)),
            None => (outcome.reason, outcome.error, outcome.detail),
        };
        self.forward(Frame::SessionStart {
            session: ended.session.clone(),
            source: Source::Dictation,
            engine: done.engine,
            streaming: done.streaming,
        })?;
        self.forward(Frame::SessionEnd {
            session: ended.session,
            reason,
            error,
            detail,
        })
    }

    fn message(&mut self, message: Message) -> Result<(), End> {
        // Without a gateway subscribers only listen: whatever they send is
        // read and ignored.
        if self.gateway.is_none() {
            return Ok(());
        }
        match message {
            Message::Text(text) => self.text(text.as_str()),
            Message::Binary(bytes) => self.binary(&bytes),
            _ => Ok(()),
        }
    }

    fn text(&mut self, text: &str) -> Result<(), End> {
        let Ok(message) = serde_json::from_str::<ClientMessage>(text) else {
            return self.refuse(
                None,
                None,
                Refusal::BadRequest,
                Some("not a message Anya understands"),
            );
        };
        match message {
            ClientMessage::Pair { code, name } => self.pair(&code, &name),
            ClientMessage::DictationStart {
                request_id,
                language,
                hotwords,
            } => self.dictation_start(request_id, language, hotwords),
            ClientMessage::DictationStop { session } => self.dictation_stop(session),
            ClientMessage::DictationCancel { session } => self.dictation_cancel(session),
            ClientMessage::Unknown => self.refuse(
                None,
                None,
                Refusal::BadRequest,
                Some("unknown message type"),
            ),
        }
    }

    fn pair(&mut self, code: &str, name: &str) -> Result<(), End> {
        let Some(gateway) = self.gateway.clone() else {
            return Ok(());
        };
        if self.who.is_some() {
            return self.refuse(None, None, Refusal::BadRequest, Some("already paired"));
        }
        let paired = match gateway.clients.pair(code, name) {
            Ok(paired) => paired,
            Err(reason) => return self.refuse(None, None, reason, None),
        };
        let Some(attached) = gateway.clients.attach(&paired.id) else {
            return self.refuse(None, None, Refusal::PairingFailed, None);
        };
        self.who = Some(attached);
        self.rescope()?;
        self.reply(&Reply::Paired {
            client: paired.id,
            token: paired.token,
            scopes: paired.scopes,
        })
    }

    /// Applies the client's scopes as they are now.
    fn rescope(&mut self) -> Result<(), End> {
        let (Some(gateway), Some(who)) = (&self.gateway, &self.who) else {
            return Ok(());
        };
        let Some(scopes) = gateway.clients.scopes_of(who.id()) else {
            return Err(End::Removed);
        };
        let may_dictate = scopes.contains(&Scope::Dictate);
        let newly = self.grant.set_scopes(scopes);
        if !may_dictate && let Some(session) = self.dictations.recording().cloned() {
            self.dictations.cancel(&session);
        }
        // A source granted now starts with its state; its sessions that
        // are already running are not replayed, later ones are sent.
        for (source, state, device) in self.hub.sources() {
            if newly.contains(&source) {
                self.forward(Frame::Source {
                    source,
                    state,
                    device,
                })?;
            }
        }
        Ok(())
    }

    fn may_dictate(&self) -> Result<(), Refusal> {
        if self.who.is_none() {
            Err(Refusal::Unauthorized)
        } else if !self.grant.scopes.contains(&Scope::Dictate) {
            Err(Refusal::Forbidden)
        } else {
            Ok(())
        }
    }

    fn dictation_start(
        &mut self,
        request_id: String,
        language: Option<String>,
        hotwords: Vec<String>,
    ) -> Result<(), End> {
        let Some(gateway) = self.gateway.clone() else {
            return Ok(());
        };
        if let Err(reason) = self.may_dictate() {
            return self.refuse(Some(request_id), None, reason, None);
        }
        match self
            .dictations
            .start(&self.hub, &gateway, language, hotwords)
        {
            Err((reason, detail)) => self.refuse(Some(request_id), None, reason, detail.as_deref()),
            Ok(started) => {
                self.grant.own.insert(started.session.clone());
                if let Some(who) = &self.who {
                    self.ledger.opened(&started.session, who.id());
                }
                self.refused_audio = false;
                self.reply(&Reply::DictationAccepted {
                    request_id,
                    session: started.session,
                    streaming: started.streaming,
                })
            }
        }
    }

    fn dictation_stop(&mut self, session: SessionId) -> Result<(), End> {
        if self.who.is_none() {
            return self.refuse(None, Some(session), Refusal::Unauthorized, None);
        }
        match self.dictations.stop(&session) {
            Ok(()) => Ok(()),
            Err(StopError::Unknown) => {
                self.refuse(None, Some(session), Refusal::UnknownSession, None)
            }
            Err(StopError::Stopped) => self.refuse(
                None,
                Some(session),
                Refusal::BadRequest,
                Some("already stopped"),
            ),
        }
    }

    fn dictation_cancel(&mut self, session: SessionId) -> Result<(), End> {
        if self.who.is_none() {
            return self.refuse(None, Some(session), Refusal::Unauthorized, None);
        }
        if self.dictations.cancel(&session) {
            Ok(())
        } else {
            self.refuse(None, Some(session), Refusal::UnknownSession, None)
        }
    }

    fn binary(&mut self, bytes: &[u8]) -> Result<(), End> {
        let refusal = if self.who.is_none() {
            Refusal::Unauthorized
        } else {
            match self.dictations.push(bytes) {
                Pushed::Taken | Pushed::Overflowed => return Ok(()),
                Pushed::NoRecording => Refusal::BadRequest,
            }
        };
        // A client that keeps sending is told once, not once per message.
        if std::mem::replace(&mut self.refused_audio, true) {
            return Ok(());
        }
        self.refuse(None, None, refusal, Some("no recording is open"))
    }

    /// The recording reached its longest length: it stops as if the client had.
    fn recording_expired(&mut self) -> Result<(), End> {
        if let Some(session) = self.dictations.recording().cloned() {
            let _ = self.dictations.stop(&session);
        }
        Ok(())
    }
}

/// What a connection receives: the sources its scopes allow, their
/// sessions, and always the sessions it opened itself.
///
/// A connection without a token on loopback holds `subscribe_continuous`
/// only: dictation sessions, their levels and the dictation source never
/// leave (docs/decisions/0007-downstream-apps.md). A paired client holds
/// the scopes the user gave it, and they can change while it is connected.
struct Grant {
    scopes: Vec<Scope>,
    /// Sessions being forwarded, and the source each belongs to.
    sessions: HashMap<SessionId, Source>,
    /// Sessions this connection opened, admitted whatever the scopes.
    own: HashSet<SessionId>,
}

impl Grant {
    fn new(scopes: Vec<Scope>) -> Grant {
        Grant {
            scopes,
            sessions: HashMap::new(),
            own: HashSet::new(),
        }
    }

    fn granted(scopes: &[Scope], source: Source) -> bool {
        match source {
            Source::Continuous => scopes.contains(&Scope::SubscribeContinuous),
            Source::Dictation => scopes.contains(&Scope::SubscribeDictation),
            Source::Unknown => false,
        }
    }

    fn admits(&mut self, frame: &Frame) -> bool {
        match frame {
            Frame::Hello { .. } => true,
            Frame::Source { source, .. } => Grant::granted(&self.scopes, *source),
            Frame::SessionStart {
                session, source, ..
            } => {
                (self.own.contains(session) || Grant::granted(&self.scopes, *source))
                    && self.sessions.insert(session.clone(), *source).is_none()
            }
            Frame::Result { session, .. } | Frame::Level { session, .. } => {
                self.sessions.contains_key(session)
            }
            Frame::SessionEnd { session, .. } => {
                self.own.remove(session);
                self.sessions.remove(session).is_some()
            }
            Frame::Unknown => false,
        }
    }

    /// Replaces the scopes. Sessions of a source no longer granted stop
    /// being forwarded, mid-session. Returns the sources granted now that
    /// were not before.
    fn set_scopes(&mut self, scopes: Vec<Scope>) -> Vec<Source> {
        let sources = [Source::Continuous, Source::Dictation];
        let before = sources.map(|s| Grant::granted(&self.scopes, s));
        self.scopes = scopes;
        let (scopes, own) = (&self.scopes, &self.own);
        self.sessions
            .retain(|id, source| own.contains(id) || Grant::granted(scopes, *source));
        sources
            .into_iter()
            .zip(before)
            .filter(|(s, was)| !was && Grant::granted(&self.scopes, *s))
            .map(|(s, _)| s)
            .collect()
    }
}
