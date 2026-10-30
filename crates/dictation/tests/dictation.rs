//! Dictation as the app and subscribers see it, with a fake microphone and
//! scripted engines on paused time.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_core::audio::{AudioError, AudioSource, Capture, Chunk, Device, DeviceKind};
use anya_core::engine::{
    Choice, Engine, EngineError, Input as EngineInput, Output, Session, SessionParams,
};
use anya_core::hub::{Hub, HubInfo, Subscription};
use anya_dictation::{
    Dictation, ERROR_HOLD, FINAL_TIMEOUT, Finished, Input, Mode, Settings, State, Status,
};
use anya_protocol::{
    EndReason, ErrorCode, Field, FieldKind, Frame, Manifest, Recognition, SessionId, Source,
    SourceState, Utterance, contract,
};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// How a fake engine behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    /// Ready, a partial per chunk, the final at once on Finish.
    Echo,
    /// Like Echo, but the final comes 5 s after Finish.
    Slow,
    /// Partials, but never a final.
    Stall,
    /// Ready, then never answers.
    Mute,
    /// Cannot connect.
    Unreachable,
    /// One partial, then the connection closes.
    HangUp,
    /// The user chose no engine.
    Missing,
    /// The engine needs an `api_key` the user did not set.
    NeedsKey,
}

struct Fake {
    manifest: Manifest,
    script: Script,
}

fn fake(script: Script) -> Fake {
    let fields = if script == Script::NeedsKey {
        vec![Field {
            key: "api_key".to_owned(),
            label: "API key".into(),
            help: Default::default(),
            kind: FieldKind::Secret,
            required: true,
            advanced: false,
        }]
    } else {
        Vec::new()
    };
    Fake {
        manifest: Manifest {
            id: "fake".to_owned(),
            title: "Fake".into(),
            blurb: Default::default(),
            docs: None,
            streaming: true,
            fields,
        },
        script,
    }
}

fn recognition(text: &str, is_final: bool) -> Recognition {
    let utterances = if text.is_empty() {
        Vec::new()
    } else {
        vec![Utterance {
            text: text.to_owned(),
            definite: is_final,
        }]
    };
    Recognition {
        text: text.to_owned(),
        utterances,
        is_final,
    }
}

impl Engine for Fake {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, _: SessionParams) -> Session {
        let (input, mut audio) = mpsc::channel(64);
        let (tx, output) = mpsc::channel(64);
        let script = self.script;
        tokio::spawn(async move {
            if script == Script::Unreachable {
                let _ = tx
                    .send(Output::Error(EngineError::Connect("refused".into())))
                    .await;
                return;
            }
            let _ = tx.send(Output::Ready).await;
            let mut text = String::new();
            while let Some(input) = audio.recv().await {
                match input {
                    EngineInput::Audio(_) if script == Script::Mute => {}
                    EngineInput::Audio(_) => {
                        text.push('a');
                        let _ = tx.send(Output::Result(recognition(&text, false))).await;
                        if script == Script::HangUp {
                            return;
                        }
                    }
                    EngineInput::Finish => {
                        match script {
                            Script::Stall | Script::Mute => std::future::pending::<()>().await,
                            Script::Slow => tokio::time::sleep(Duration::from_secs(5)).await,
                            _ => {}
                        }
                        if !text.is_empty() {
                            text.push('.');
                        }
                        let _ = tx.send(Output::Result(recognition(&text, true))).await;
                        return;
                    }
                }
            }
        });
        Session { input, output }
    }
}

/// A microphone that records only what the test says.
#[derive(Default)]
struct Mic {
    state: Mutex<MicState>,
}

#[derive(Default)]
struct MicState {
    opened: Vec<(Option<String>, DeviceKind)>,
    live: Option<mpsc::Sender<Result<Chunk, AudioError>>>,
    deny: bool,
}

impl AudioSource for Mic {
    fn devices(&self) -> Result<Vec<Device>, AudioError> {
        Ok(Vec::new())
    }

    fn open(&self, device: Option<&str>, default: DeviceKind) -> Result<Capture, AudioError> {
        let mut state = self.state.lock().expect("mic");
        if state.deny {
            return Err(AudioError::Denied("not allowed".into()));
        }
        state.opened.push((device.map(str::to_owned), default));
        let (tx, chunks) = mpsc::channel(64);
        state.live = Some(tx);
        Ok(Capture { chunks })
    }
}

impl Mic {
    fn send(&self, item: Result<Chunk, AudioError>) -> bool {
        let live = self.state.lock().expect("mic").live.clone();
        live.is_some_and(|tx| tx.try_send(item).is_ok())
    }

    fn opened(&self) -> Vec<(Option<String>, DeviceKind)> {
        self.state.lock().expect("mic").opened.clone()
    }

    /// Whether the last capture was let go of.
    fn released(&self) -> bool {
        let state = self.state.lock().expect("mic");
        state.live.as_ref().is_none_or(mpsc::Sender::is_closed)
    }

    fn deny(&self, deny: bool) {
        self.state.lock().expect("mic").deny = deny;
    }
}

fn loud() -> Chunk {
    Chunk([0x00, 0x40].repeat(320))
}

/// Lets every task run until all wait on something.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

struct Rig {
    hub: Hub,
    sub: Subscription,
    seen: Vec<Frame>,
    mic: Arc<Mic>,
    script: Arc<Mutex<Script>>,
    dictation: Dictation,
    status: watch::Receiver<Status>,
    finished: mpsc::UnboundedReceiver<Finished>,
}

impl Rig {
    fn new(settings: Settings, script: Script) -> Rig {
        let hub = Hub::new(HubInfo {
            version: "0.1.0".to_owned(),
            session_prefix: "anya-test".to_owned(),
        });
        let sub = hub.subscribe();
        let mic = Arc::new(Mic::default());
        let script = Arc::new(Mutex::new(script));
        let chosen = Arc::clone(&script);
        let choice: Choice = Arc::new(move || match *chosen.lock().expect("script") {
            Script::Missing => Err(EngineError::NotConfigured("engine".into())),
            script => Ok((
                Arc::new(fake(script)) as Arc<dyn Engine>,
                SessionParams::default(),
            )),
        });
        let audio: Arc<dyn AudioSource> = Arc::clone(&mic) as Arc<dyn AudioSource>;
        let (dictation, status, finished) =
            anya_dictation::spawn(hub.clone(), audio, choice, settings);
        Rig {
            hub,
            sub,
            seen: Vec::new(),
            mic,
            script,
            dictation,
            status,
            finished,
        }
    }

    fn script(&self, script: Script) {
        *self.script.lock().expect("script") = script;
    }

    async fn input(&self, input: Input) {
        self.dictation.input(input);
        settle().await;
    }

    /// Press and release.
    async fn tap(&self, key: u32) {
        self.input(Input::Press(key)).await;
        self.input(Input::Release(key)).await;
    }

    async fn speak(&self) {
        assert!(self.mic.send(Ok(loud())), "the microphone is not open");
        settle().await;
    }

    fn state(&self) -> State {
        self.status.borrow().state.clone()
    }

    fn finishing(&self) -> usize {
        self.status.borrow().finishing
    }

    async fn until(&mut self, done: impl Fn(&Status) -> bool) -> Status {
        tokio::time::timeout(Duration::from_secs(120), self.status.wait_for(|s| done(s)))
            .await
            .expect("status never came")
            .expect("dictation stopped")
            .clone()
    }

    async fn idle(&mut self) {
        self.until(|s| s.state == State::Idle && s.finishing == 0)
            .await;
    }

    fn finished(&mut self) -> Vec<Finished> {
        let mut out = Vec::new();
        while let Ok(f) = self.finished.try_recv() {
            out.push(f);
        }
        out
    }

    /// Every frame a subscriber received so far, checked against the promises.
    fn frames(&mut self) -> Vec<Frame> {
        self.seen.append(&mut self.sub.snapshot);
        while let Ok(f) = self.sub.frames.try_recv() {
            self.seen.push(f);
        }
        assert_eq!(contract::check(&self.seen), Ok(()));
        self.seen.clone()
    }

    /// Nothing more is emitted, however long we wait.
    async fn quiet(&mut self) {
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(self.finished(), Vec::new());
    }
}

fn starts(frames: &[Frame]) -> Vec<SessionId> {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::SessionStart { session, .. } => Some(session.clone()),
            _ => None,
        })
        .collect()
}

fn ends(frames: &[Frame]) -> Vec<(EndReason, Option<ErrorCode>)> {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::SessionEnd { reason, error, .. } => Some((*reason, *error)),
            _ => None,
        })
        .collect()
}

/// The device of every `listening` frame of the dictation source.
fn listening(frames: &[Frame]) -> Vec<Option<String>> {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::Source {
                source: Source::Dictation,
                state: SourceState::Listening,
                device,
            } => Some(device.clone()),
            _ => None,
        })
        .collect()
}

fn toggle(stop_delay_ms: u64) -> Settings {
    Settings {
        mode: Mode::Toggle,
        stop_delay: Duration::from_millis(stop_delay_ms),
        ..Settings::default()
    }
}

fn hold(stop_delay_ms: u64) -> Settings {
    Settings {
        mode: Mode::Hold,
        stop_delay: Duration::from_millis(stop_delay_ms),
        ..Settings::default()
    }
}

fn failed(error: ErrorCode, detail: Option<&str>) -> State {
    State::Failed {
        error,
        detail: detail.map(str::to_owned),
    }
}

#[tokio::test(start_paused = true)]
async fn toggle_starts_on_a_press_and_stops_on_the_next() {
    let mut rig = Rig::new(toggle(0), Script::Echo);
    assert_eq!(
        rig.hub.sources(),
        [(Source::Dictation, SourceState::Idle, None)]
    );

    rig.input(Input::Press(1)).await;
    assert_eq!(rig.state(), State::Recording);
    assert_eq!(
        rig.hub.sources(),
        [(Source::Dictation, SourceState::Listening, None)]
    );
    rig.speak().await;
    // Key repeat while the key is down changes nothing.
    rig.input(Input::Press(1)).await;
    rig.speak().await;
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), State::Recording);
    assert!(!rig.mic.released());

    rig.input(Input::Press(1)).await;
    assert!(rig.mic.released());
    assert_eq!(
        rig.hub.sources(),
        [(Source::Dictation, SourceState::Idle, None)]
    );
    rig.idle().await;

    let frames = rig.frames();
    let session = starts(&frames);
    assert_eq!(session.len(), 1);
    assert_eq!(
        rig.finished(),
        [Finished {
            session: session[0].clone(),
            text: "aa.".to_owned(),
            audio_ms: 40,
        }]
    );
    assert_eq!(ends(&frames), [(EndReason::Final, None)]);
    // The device is released before the session ends.
    let idle = frames.iter().rposition(|f| {
        matches!(
            f,
            Frame::Source {
                state: SourceState::Idle,
                ..
            }
        )
    });
    let end = frames
        .iter()
        .position(|f| matches!(f, Frame::SessionEnd { .. }));
    assert!(idle < end);
    rig.quiet().await;
    assert_eq!(rig.mic.opened().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn hold_records_until_every_hotkey_is_released() {
    let mut rig = Rig::new(hold(0), Script::Echo);
    rig.input(Input::Press(1)).await;
    assert_eq!(rig.state(), State::Recording);
    rig.speak().await;
    rig.input(Input::Press(2)).await;
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), State::Recording);
    assert!(!rig.mic.released());
    rig.speak().await;

    rig.input(Input::Release(2)).await;
    assert!(rig.mic.released());
    rig.idle().await;
    let texts: Vec<String> = rig.finished().into_iter().map(|f| f.text).collect();
    assert_eq!(texts, ["aa."]);
    assert_eq!(rig.mic.opened().len(), 1);
    // A stray release starts nothing.
    rig.input(Input::Release(3)).await;
    assert_eq!(rig.state(), State::Idle);
    rig.frames();
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn the_stop_delay_keeps_recording_and_a_press_resumes() {
    let mut rig = Rig::new(hold(500), Script::Echo);
    rig.input(Input::Press(1)).await;
    rig.speak().await;
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), State::Stopping);
    assert!(!rig.mic.released());

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.state(), State::Stopping);
    rig.input(Input::Press(1)).await;
    assert_eq!(rig.state(), State::Recording);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(rig.state(), State::Recording);
    rig.speak().await;

    let released = Instant::now();
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), State::Stopping);
    tokio::time::sleep(Duration::from_millis(400)).await;
    // Audio in the stop delay still counts.
    rig.speak().await;
    assert!(!rig.mic.released());
    rig.until(|s| s.state == State::Idle).await;
    assert!(released.elapsed() >= Duration::from_millis(500));
    assert!(rig.mic.released());
    rig.idle().await;

    let texts: Vec<String> = rig.finished().into_iter().map(|f| f.text).collect();
    assert_eq!(texts, ["aaa."]);
    assert_eq!(rig.mic.opened().len(), 1);
    rig.frames();
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn a_toggle_press_in_the_stop_delay_starts_a_new_recording() {
    let mut rig = Rig::new(toggle(500), Script::Echo);
    rig.tap(1).await;
    rig.speak().await;
    rig.tap(1).await;
    assert_eq!(rig.state(), State::Stopping);

    rig.tap(1).await;
    assert_eq!(rig.state(), State::Recording);
    assert_eq!(rig.mic.opened().len(), 2);
    rig.speak().await;
    rig.speak().await;
    rig.tap(1).await;
    assert_eq!(rig.state(), State::Stopping);
    rig.idle().await;

    let frames = rig.frames();
    let sessions = starts(&frames);
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        rig.finished(),
        [
            Finished {
                session: sessions[0].clone(),
                text: "a.".to_owned(),
                audio_ms: 20,
            },
            Finished {
                session: sessions[1].clone(),
                text: "aa.".to_owned(),
                audio_ms: 40,
            },
        ]
    );
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn rapid_toggles_finish_every_session_once() {
    let mut rig = Rig::new(toggle(0), Script::Slow);
    for words in 1..=3 {
        rig.tap(1).await;
        assert_eq!(rig.state(), State::Recording);
        for _ in 0..words {
            rig.speak().await;
        }
        rig.tap(1).await;
    }
    {
        let status = rig.status.borrow();
        assert_eq!(status.state, State::Idle);
        assert_eq!(status.finishing, 3);
        assert_eq!(status.waiting.len(), 3, "each waiting session is named");
        assert_eq!(status.recording, None);
    }

    // Presses faster than anything can run are handled in order.
    for input in [
        Input::Press(1),
        Input::Release(1),
        Input::Press(1),
        Input::Release(1),
        Input::Press(1),
    ] {
        rig.dictation.input(input);
    }
    settle().await;
    assert_eq!(rig.state(), State::Recording);
    assert_eq!(rig.finishing(), 4);
    assert_eq!(rig.mic.opened().len(), 5);
    rig.speak().await;
    rig.input(Input::Release(1)).await;
    rig.tap(1).await;
    rig.idle().await;

    let frames = rig.frames();
    let sessions = starts(&frames);
    assert_eq!(sessions.len(), 5);
    let finished = rig.finished();
    let texts: Vec<&str> = finished.iter().map(|f| f.text.as_str()).collect();
    assert_eq!(texts, ["a.", "aa.", "aaa.", "a."]);
    let from: Vec<&SessionId> = finished.iter().map(|f| &f.session).collect();
    // The session that heard nothing yields nothing.
    assert_eq!(
        from,
        [&sessions[0], &sessions[1], &sessions[2], &sessions[4]]
    );
    assert_eq!(ends(&frames), [(EndReason::Final, None); 5]);
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn cancel_throws_the_recording_away() {
    let mut rig = Rig::new(toggle(0), Script::Echo);
    rig.tap(1).await;
    rig.speak().await;
    rig.input(Input::Cancel).await;
    assert_eq!(*rig.status.borrow(), Status::default());
    assert!(rig.mic.released());
    assert_eq!(
        rig.hub.sources(),
        [(Source::Dictation, SourceState::Idle, None)]
    );

    // Also in the stop delay.
    rig.dictation.configure(hold(500));
    rig.input(Input::Press(1)).await;
    rig.speak().await;
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), State::Stopping);
    rig.input(Input::Cancel).await;
    assert_eq!(rig.state(), State::Idle);
    assert!(rig.mic.released());

    rig.quiet().await;
    let frames = rig.frames();
    assert_eq!(ends(&frames), [(EndReason::Cancelled, None); 2]);
}

#[tokio::test(start_paused = true)]
async fn cancel_while_finishing_suppresses_the_text() {
    let mut rig = Rig::new(toggle(0), Script::Slow);
    rig.tap(1).await;
    rig.speak().await;
    rig.speak().await;
    rig.tap(1).await;
    assert_eq!(rig.finishing(), 1);

    rig.input(Input::Cancel).await;
    assert_eq!(rig.finishing(), 0);

    // The next session is not affected.
    rig.tap(1).await;
    rig.speak().await;
    rig.tap(1).await;
    rig.idle().await;

    let frames = rig.frames();
    let sessions = starts(&frames);
    assert_eq!(
        rig.finished(),
        [Finished {
            session: sessions[1].clone(),
            text: "a.".to_owned(),
            audio_ms: 20,
        }]
    );
    assert_eq!(
        ends(&frames),
        [(EndReason::Cancelled, None), (EndReason::Final, None)]
    );
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn retry_after_a_failure_starts_a_new_recording() {
    let mut rig = Rig::new(toggle(0), Script::Unreachable);
    // Nothing to retry yet.
    rig.input(Input::Retry).await;
    assert_eq!(rig.state(), State::Idle);
    assert!(rig.mic.opened().is_empty());

    rig.tap(1).await;
    assert_eq!(rig.state(), failed(ErrorCode::Connect, Some("refused")));
    assert!(rig.mic.released());
    assert_eq!(
        rig.hub.sources(),
        [(Source::Dictation, SourceState::Idle, None)]
    );
    assert!(starts(&rig.frames()).is_empty());

    rig.script(Script::Echo);
    rig.input(Input::Retry).await;
    assert_eq!(rig.state(), State::Recording);
    assert_eq!(rig.mic.opened().len(), 2);
    // Retry does nothing while recording.
    rig.input(Input::Retry).await;
    assert_eq!(rig.mic.opened().len(), 2);
    rig.speak().await;
    rig.tap(1).await;
    rig.idle().await;

    let texts: Vec<String> = rig.finished().into_iter().map(|f| f.text).collect();
    assert_eq!(texts, ["a."]);
    assert_eq!(ends(&rig.frames()), [(EndReason::Final, None)]);
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn a_failure_shows_for_the_hold_and_cancel_dismisses_it() {
    let mut rig = Rig::new(toggle(0), Script::Unreachable);
    let pressed = Instant::now();
    rig.tap(1).await;
    assert_eq!(rig.state(), failed(ErrorCode::Connect, Some("refused")));
    rig.until(|s| s.state == State::Idle).await;
    let shown = pressed.elapsed();
    assert!(shown >= ERROR_HOLD, "{shown:?}");
    assert!(shown < ERROR_HOLD + Duration::from_millis(100), "{shown:?}");

    // Past the hold there is nothing to retry.
    rig.input(Input::Retry).await;
    assert_eq!(rig.mic.opened().len(), 1);

    rig.tap(1).await;
    assert_eq!(rig.state(), failed(ErrorCode::Connect, Some("refused")));
    rig.input(Input::Cancel).await;
    assert_eq!(rig.state(), State::Idle);
    rig.frames();
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn a_timeout_fails_and_keeps_what_was_recognized() {
    let mut rig = Rig::new(toggle(0), Script::Stall);
    rig.tap(1).await;
    rig.speak().await;
    rig.tap(1).await;
    assert_eq!(rig.finishing(), 1);
    tokio::time::sleep(FINAL_TIMEOUT - Duration::from_secs(1)).await;
    assert_eq!(rig.finishing(), 1);
    assert_eq!(rig.finished(), Vec::new());

    let status = rig.until(|s| s.finishing == 0).await;
    assert_eq!(status.state, failed(ErrorCode::Timeout, None));
    let texts: Vec<String> = rig.finished().into_iter().map(|f| f.text).collect();
    assert_eq!(texts, ["a"]);

    // Nothing recognized: the failure alone.
    rig.script(Script::Mute);
    rig.tap(1).await;
    rig.speak().await;
    rig.tap(1).await;
    assert_eq!(rig.state(), State::Idle);
    let status = rig.until(|s| s.finishing == 0).await;
    assert_eq!(status.state, failed(ErrorCode::Timeout, None));
    assert_eq!(rig.finished(), Vec::new());

    assert_eq!(
        ends(&rig.frames()),
        [(EndReason::Error, Some(ErrorCode::Timeout)); 2]
    );
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn an_unconfigured_engine_fails_before_the_device_opens() {
    let mut rig = Rig::new(toggle(0), Script::Missing);
    rig.tap(1).await;
    assert_eq!(
        rig.state(),
        failed(ErrorCode::NotConfigured, Some("engine"))
    );

    rig.script(Script::NeedsKey);
    rig.input(Input::Retry).await;
    assert_eq!(
        rig.state(),
        failed(ErrorCode::NotConfigured, Some("api_key"))
    );

    assert!(rig.mic.opened().is_empty());
    assert!(listening(&rig.frames()).is_empty());
    rig.quiet().await;
}

#[tokio::test(start_paused = true)]
async fn capture_failures_show_as_audio() {
    let mut rig = Rig::new(toggle(0), Script::Echo);
    rig.mic.deny(true);
    rig.tap(1).await;
    assert!(matches!(
        rig.state(),
        State::Failed { error: ErrorCode::Audio, detail: Some(d) } if d.contains("denied")
    ));
    assert!(listening(&rig.frames()).is_empty());

    rig.mic.deny(false);
    rig.input(Input::Retry).await;
    assert_eq!(rig.state(), State::Recording);
    rig.speak().await;
    assert!(rig.mic.send(Err(AudioError::Backend("unplugged".into()))));
    settle().await;
    assert!(matches!(
        rig.state(),
        State::Failed { error: ErrorCode::Audio, detail: Some(d) } if d.contains("unplugged")
    ));
    assert!(rig.mic.released());
    assert_eq!(
        rig.hub.sources(),
        [(Source::Dictation, SourceState::Idle, None)]
    );
    rig.input(Input::Cancel).await;
    assert_eq!(rig.state(), State::Idle);

    rig.quiet().await;
    assert_eq!(ends(&rig.frames()), [(EndReason::Closed, None)]);
}

#[tokio::test(start_paused = true)]
async fn an_engine_hanging_up_while_recording_fails_as_connect() {
    let mut rig = Rig::new(hold(0), Script::HangUp);
    rig.input(Input::Press(1)).await;
    rig.speak().await;
    assert_eq!(rig.state(), failed(ErrorCode::Connect, None));
    assert!(rig.mic.released());
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), failed(ErrorCode::Connect, None));

    rig.quiet().await;
    assert_eq!(ends(&rig.frames()), [(EndReason::Closed, None)]);
}

#[tokio::test(start_paused = true)]
async fn capture_failing_in_the_stop_delay_keeps_the_recording() {
    let mut rig = Rig::new(hold(500), Script::Echo);
    rig.input(Input::Press(1)).await;
    rig.speak().await;
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), State::Stopping);
    assert!(rig.mic.send(Err(AudioError::Backend("unplugged".into()))));
    settle().await;
    assert!(rig.mic.released());
    let status = rig.until(|s| s.finishing == 0).await;
    assert!(matches!(
        status.state,
        State::Failed {
            error: ErrorCode::Audio,
            ..
        }
    ));
    let texts: Vec<String> = rig.finished().into_iter().map(|f| f.text).collect();
    assert_eq!(texts, ["a."]);

    rig.quiet().await;
    assert_eq!(ends(&rig.frames()), [(EndReason::Final, None)]);
}

#[tokio::test(start_paused = true)]
async fn settings_apply_to_the_next_recording() {
    let mut rig = Rig::new(toggle(0), Script::Echo);
    rig.input(Input::Press(1)).await;
    rig.dictation.configure(Settings {
        device: Some("usb".to_owned()),
        ..hold(0)
    });
    rig.speak().await;
    // The recording that is running keeps its device.
    // Without a device, dictation asks for the microphone, never system sound.
    assert_eq!(rig.mic.opened(), [(None, DeviceKind::Microphone)]);
    // The mode changed: the press before it is forgotten, and this release
    // stops nothing.
    rig.input(Input::Release(1)).await;
    assert_eq!(rig.state(), State::Recording);
    rig.input(Input::Press(1)).await;
    rig.input(Input::Release(1)).await;
    rig.idle().await;

    rig.input(Input::Press(2)).await;
    assert_eq!(
        rig.mic.opened(),
        [
            (None, DeviceKind::Microphone),
            (Some("usb".to_owned()), DeviceKind::Microphone)
        ]
    );
    assert_eq!(
        rig.hub.sources(),
        [(
            Source::Dictation,
            SourceState::Listening,
            Some("usb".to_owned())
        )]
    );
    rig.speak().await;
    rig.input(Input::Release(2)).await;
    rig.idle().await;

    let texts: Vec<String> = rig.finished().into_iter().map(|f| f.text).collect();
    assert_eq!(texts, ["a.", "a."]);
    assert_eq!(listening(&rig.frames()), [None, Some("usb".to_owned())]);
    rig.quiet().await;
}
