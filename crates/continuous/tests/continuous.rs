//! Continuous transcription driven by scripted capture and scripted engines,
//! under paused time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_continuous::{Continuous, Settings, spawn};
use anya_core::audio::{
    AudioError, AudioSource, CHUNK_BYTES, CHUNK_MS, Capture, Chunk, Device, DeviceKind,
};
use anya_core::engine::{Choice, Engine, EngineError, Input, Output, Session, SessionParams};
use anya_core::hub::{Hub, HubInfo};
use anya_protocol::{Frame, Manifest, Recognition, Source, SourceState, Text, Utterance, contract};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Instant, sleep, timeout};

const WAIT: Duration = Duration::from_secs(30);

type Sink = mpsc::Sender<Result<Chunk, AudioError>>;

/// A device the fake opened.
struct Mic {
    device: Option<String>,
    kind: DeviceKind,
    tx: Sink,
    at: Instant,
}

/// Capture whose chunks the test sends. Two devices: "mic-1" (Desk Mic) and
/// "loop-1" (BlackHole 2ch).
struct FakeAudio {
    opened: mpsc::UnboundedSender<Mic>,
    fail_opens: Mutex<u32>,
    handed: Mutex<Vec<Sink>>,
    overlapped: AtomicBool,
}

impl AudioSource for FakeAudio {
    fn devices(&self) -> Result<Vec<Device>, AudioError> {
        Ok(vec![
            Device {
                id: "mic-1".to_owned(),
                name: "Desk Mic".to_owned(),
                kind: DeviceKind::Microphone,
            },
            Device {
                id: "loop-1".to_owned(),
                name: "BlackHole 2ch".to_owned(),
                kind: DeviceKind::Loopback,
            },
        ])
    }

    fn open(&self, device: Option<&str>, kind: DeviceKind) -> Result<Capture, AudioError> {
        {
            let mut fails = self.fail_opens.lock().expect("lock");
            if *fails > 0 {
                *fails -= 1;
                return Err(AudioError::Backend("device busy".to_owned()));
            }
        }
        let (tx, chunks) = mpsc::channel(1024);
        let mut handed = self.handed.lock().expect("lock");
        if handed.iter().any(|t| !t.is_closed()) {
            self.overlapped.store(true, Ordering::SeqCst);
        }
        handed.push(tx.clone());
        let _ = self.opened.send(Mic {
            device: device.map(str::to_owned),
            kind,
            tx,
            at: Instant::now(),
        });
        Ok(Capture { chunks })
    }
}

/// A session the scripted engine opened: the test reads its input and
/// writes its output.
struct Opened {
    params: SessionParams,
    input: mpsc::Receiver<Input>,
    output: mpsc::Sender<Output>,
    at: Instant,
}

#[derive(Debug, PartialEq)]
enum In {
    A(i16),
    Finish,
}

fn input(i: Input) -> In {
    match i {
        Input::Audio(c) => In::A(tag(&c)),
        Input::Finish => In::Finish,
    }
}

impl Opened {
    fn drain(&mut self) -> Vec<In> {
        let mut got = Vec::new();
        while let Ok(i) = self.input.try_recv() {
            got.push(input(i));
        }
        got
    }

    async fn until_finish(&mut self) -> Vec<In> {
        let mut got = Vec::new();
        loop {
            let i = timeout(WAIT, self.input.recv())
                .await
                .expect("finish in time")
                .expect("session input open");
            let i = input(i);
            let done = i == In::Finish;
            got.push(i);
            if done {
                return got;
            }
        }
    }

    fn reply(&self, text: &str, is_final: bool) {
        let recognition = Recognition {
            text: text.to_owned(),
            utterances: vec![Utterance {
                text: text.to_owned(),
                definite: is_final,
            }],
            is_final,
        };
        self.output
            .try_send(Output::Result(recognition))
            .expect("engine output open");
    }
}

/// An engine whose sessions the test drives. The first `fail` opens fail to
/// connect.
struct Script {
    manifest: Manifest,
    fail: Mutex<u32>,
    attempts: Mutex<Vec<Instant>>,
    opened: mpsc::UnboundedSender<Opened>,
}

impl Engine for Script {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, params: SessionParams) -> Session {
        let (input, input_rx) = mpsc::channel(4096);
        let (output_tx, output) = mpsc::channel(64);
        self.attempts.lock().expect("lock").push(Instant::now());
        let failing = {
            let mut fail = self.fail.lock().expect("lock");
            let failing = *fail > 0;
            *fail = fail.saturating_sub(1);
            failing
        };
        if failing {
            let refused = EngineError::Connect("connection refused".to_owned());
            let _ = output_tx.try_send(Output::Error(refused));
        } else {
            let _ = output_tx.try_send(Output::Ready);
            let _ = self.opened.send(Opened {
                params,
                input: input_rx,
                output: output_tx,
                at: Instant::now(),
            });
        }
        Session { input, output }
    }
}

struct Rig {
    hub: Hub,
    frames: broadcast::Receiver<Frame>,
    log: Vec<Frame>,
    audio: Arc<FakeAudio>,
    mics: mpsc::UnboundedReceiver<Mic>,
    mic: Option<Sink>,
    engine: Arc<Script>,
    sessions: mpsc::UnboundedReceiver<Opened>,
    params: watch::Sender<u64>,
    hotwords: Arc<Mutex<Vec<String>>>,
    continuous: Continuous,
}

impl Rig {
    fn new(streaming: bool, engine_fails: u32, open_fails: u32) -> Rig {
        let hub = Hub::new(HubInfo {
            version: "0.0.0".to_owned(),
            session_prefix: "test".to_owned(),
        });
        let sub = hub.subscribe();
        let (mic_tx, mics) = mpsc::unbounded_channel();
        let audio = Arc::new(FakeAudio {
            opened: mic_tx,
            fail_opens: Mutex::new(open_fails),
            handed: Mutex::default(),
            overlapped: AtomicBool::new(false),
        });
        let (opened, sessions) = mpsc::unbounded_channel();
        let engine = Arc::new(Script {
            manifest: Manifest {
                id: "script".to_owned(),
                title: Text::from("Script"),
                blurb: Text::default(),
                docs: None,
                streaming,
                fields: Vec::new(),
            },
            fail: Mutex::new(engine_fails),
            attempts: Mutex::default(),
            opened,
        });
        let hotwords: Arc<Mutex<Vec<String>>> = Arc::default();
        let choice: Choice = {
            let engine = Arc::clone(&engine);
            let hotwords = Arc::clone(&hotwords);
            Arc::new(move || {
                let hotwords = hotwords.lock().expect("lock").clone();
                let engine: Arc<dyn Engine> = engine.clone();
                Ok((
                    engine,
                    SessionParams {
                        hotwords,
                        ..SessionParams::default()
                    },
                ))
            })
        };
        let (params, params_rx) = watch::channel(0);
        let continuous = spawn(hub.clone(), audio.clone(), choice, params_rx);
        Rig {
            hub,
            frames: sub.frames,
            log: sub.snapshot,
            audio,
            mics,
            mic: None,
            engine,
            sessions,
            params,
            hotwords,
            continuous,
        }
    }

    /// The next device opened, once the controller has published it; chunks
    /// go to it from now on.
    async fn mic(&mut self) -> Mic {
        let mic = timeout(WAIT, self.mics.recv())
            .await
            .expect("device opened in time")
            .expect("fake alive");
        sleep(Duration::from_millis(1)).await;
        self.mic = Some(mic.tx.clone());
        mic
    }

    /// Captures one chunk and lets 40 ms pass, as a device does.
    async fn say(&mut self, chunk: Chunk) {
        if let Some(tx) = &self.mic {
            let _ = tx.send(Ok(chunk)).await;
        }
        sleep(Duration::from_millis(u64::from(CHUNK_MS))).await;
        self.collect();
    }

    async fn session(&mut self) -> Opened {
        timeout(WAIT, self.sessions.recv())
            .await
            .expect("session opened in time")
            .expect("engine alive")
    }

    fn no_session(&mut self) {
        assert!(self.sessions.try_recv().is_err(), "no session expected");
    }

    fn collect(&mut self) {
        loop {
            match self.frames.try_recv() {
                Ok(frame) => self.log.push(frame),
                Err(TryRecvError::Empty | TryRecvError::Closed) => return,
                Err(TryRecvError::Lagged(n)) => panic!("missed {n} frames"),
            }
        }
    }

    /// Every frame so far but `level`, in words.
    fn events(&mut self) -> Vec<String> {
        self.collect();
        self.log.iter().filter_map(describe).collect()
    }

    fn check(&mut self) {
        self.collect();
        contract::check(&self.log).expect("frames keep the promises");
    }
}

fn describe(frame: &Frame) -> Option<String> {
    Some(match frame {
        Frame::Hello { .. } => "hello".to_owned(),
        Frame::Source {
            source,
            state,
            device,
        } => format!("{source:?} {state:?} {}", device.as_deref().unwrap_or("-")),
        Frame::SessionStart { source, .. } => format!("start {source:?}"),
        Frame::Result { recognition, .. } => format!(
            "result {} {}",
            recognition.text,
            if recognition.is_final {
                "final"
            } else {
                "partial"
            }
        ),
        Frame::Level { .. } => return None,
        Frame::SessionEnd { reason, error, .. } => format!("end {reason:?} {error:?}"),
        Frame::Unknown => "unknown".to_owned(),
    })
}

fn settings(device: Option<&str>, silence_gate: bool) -> Settings {
    Settings {
        device: device.map(str::to_owned),
        silence_gate,
        final_timeout: Duration::from_secs(2),
    }
}

/// A 40 ms chunk of constant amplitude, which doubles as its tag. 1..=120 is
/// quiet (below -48 dBFS), 155..=195 a murmur (between -47 and -44 dBFS),
/// 1000 and up speech.
fn chunk(amplitude: i16) -> Chunk {
    Chunk(
        std::iter::repeat_n(amplitude, CHUNK_BYTES / 2)
            .flat_map(i16::to_le_bytes)
            .collect(),
    )
}

fn tag(c: &Chunk) -> i16 {
    i16::from_le_bytes([c.0[0], c.0[1]])
}

fn audio(tags: impl IntoIterator<Item = i16>) -> Vec<In> {
    tags.into_iter().map(In::A).collect()
}

fn near(got: Duration, want: Duration) -> bool {
    got.abs_diff(want) < Duration::from_millis(5)
}

#[tokio::test(start_paused = true)]
async fn speech_opens_a_session_with_preroll_and_silence_finishes_it() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), true));
    let mic = rig.mic().await;
    assert_eq!(mic.device.as_deref(), Some("mic-1"));
    for t in 1..=20 {
        rig.say(chunk(t)).await;
    }
    rig.no_session();
    for t in 1001..=1010 {
        rig.say(chunk(t)).await;
    }
    let mut s = rig.session().await;
    // 1.2 s of silence: 30 chunks, the last one ends the utterance.
    for t in 21..=50 {
        rig.say(chunk(t)).await;
    }
    // 600 ms of preroll is the last 15 quiet chunks.
    let mut want = audio(6..=20);
    want.extend(audio(1001..=1010));
    want.extend(audio(21..=50));
    want.push(In::Finish);
    assert_eq!(s.until_finish().await, want);
    s.reply("hello there", true);
    sleep(Duration::from_millis(100)).await;
    rig.no_session();
    assert_eq!(
        rig.events(),
        [
            "hello",
            "Continuous Idle -",
            "Continuous Listening Desk Mic",
            "start Continuous",
            "result hello there final",
            "end Final None",
        ]
    );
    assert!(rig.log.iter().any(|f| matches!(f, Frame::Level { .. })));
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn the_gate_opens_at_onset_and_closes_below_release() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), true));
    rig.mic().await;
    // A murmur under the onset level opens nothing.
    for t in 155..=195 {
        rig.say(chunk(t)).await;
    }
    rig.no_session();
    for t in 1001..=1003 {
        rig.say(chunk(t)).await;
    }
    let mut s = rig.session().await;
    // Above the release level, a murmur keeps the utterance going.
    for _ in 0..40 {
        rig.say(chunk(160)).await;
    }
    let got = s.drain();
    assert!(!got.contains(&In::Finish));
    assert_eq!(got[..15], audio(181..=195)[..]);
    assert_eq!(got.len(), 15 + 3 + 40);
    for t in 1..=29 {
        rig.say(chunk(t)).await;
    }
    assert!(!s.drain().contains(&In::Finish));
    rig.say(chunk(30)).await;
    assert_eq!(s.drain(), [In::A(30), In::Finish]);
    s.reply("ok", true);
    sleep(Duration::from_millis(100)).await;
    rig.no_session();
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn a_quiet_room_uploads_nothing() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), true));
    rig.mic().await;
    for i in 0..750 {
        rig.say(chunk(if i % 2 == 0 { 50 } else { 190 })).await;
    }
    rig.no_session();
    assert!(rig.engine.attempts.lock().expect("lock").is_empty());
    assert_eq!(
        rig.events(),
        [
            "hello",
            "Continuous Idle -",
            "Continuous Listening Desk Mic"
        ]
    );
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn a_late_final_result_times_out_and_the_next_utterance_opens_anew() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), true));
    rig.mic().await;
    for t in 1001..=1003 {
        rig.say(chunk(t)).await;
    }
    let mut first = rig.session().await;
    for t in 1..=30 {
        rig.say(chunk(t)).await;
    }
    assert_eq!(first.until_finish().await.last(), Some(&In::Finish));
    // The engine never answers: past two seconds the session ends.
    for t in 1..=60 {
        rig.say(chunk(t)).await;
    }
    for t in 1004..=1006 {
        rig.say(chunk(t)).await;
    }
    let mut second = rig.session().await;
    sleep(Duration::from_millis(40)).await;
    assert_eq!(second.drain()[15..], audio(1004..=1006)[..]);
    assert_eq!(
        rig.events()[3..],
        [
            "start Continuous",
            "end Error Some(Timeout)",
            "start Continuous"
        ]
    );
    rig.check();
    drop(first);
}

#[tokio::test(start_paused = true)]
async fn without_the_gate_a_session_reconnects_with_backoff_and_replays_recent_audio() {
    let mut rig = Rig::new(true, 6, 0);
    rig.continuous.set(true, settings(None, false));
    let mic = rig.mic().await;
    assert_eq!(mic.device, None);
    assert_eq!(mic.kind, DeviceKind::Loopback);
    let mut t = 0;
    let mut s = loop {
        t += 1;
        rig.say(chunk(t)).await;
        if let Ok(s) = rig.sessions.try_recv() {
            break s;
        }
        assert!(t < 1000, "no session connected");
    };
    let attempts = rig.engine.attempts.lock().expect("lock").clone();
    assert_eq!(attempts.len(), 7);
    assert!(near(attempts[0] - mic.at, Duration::ZERO));
    let gaps: Vec<Duration> = attempts.windows(2).map(|w| w[1] - w[0]).collect();
    let want = [500, 1000, 2000, 4000, 5000, 5000].map(Duration::from_millis);
    assert!(
        gaps.iter().zip(want).all(|(&g, w)| near(g, w)),
        "gaps {gaps:?}"
    );
    assert!(near(s.at - attempts[6], Duration::ZERO));

    // The new session hears about the last five seconds first, then live audio.
    let newest = t;
    for _ in 0..10 {
        t += 1;
        rig.say(chunk(t)).await;
    }
    let got: Vec<i16> = s
        .drain()
        .into_iter()
        .map(|i| match i {
            In::A(t) => t,
            In::Finish => panic!("no finish expected"),
        })
        .collect();
    assert_eq!(got.last(), Some(&t));
    assert!(
        got.windows(2).all(|w| w[1] == w[0] + 1),
        "in order: {got:?}"
    );
    let replayed = newest - got[0] + 1;
    assert!((120..=126).contains(&replayed), "replayed {replayed}");

    // An engine failing is not capture failing: the device stayed open.
    assert!(rig.mics.try_recv().is_err());
    assert_eq!(
        rig.events(),
        [
            "hello",
            "Continuous Idle -",
            "Continuous Listening -",
            "start Continuous"
        ]
    );
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn a_long_session_resets_the_backoff() {
    let mut rig = Rig::new(true, 2, 0);
    rig.continuous.set(true, settings(Some("mic-1"), false));
    rig.mic().await;
    let mut t = 0;
    let first = loop {
        t += 1;
        rig.say(chunk(t)).await;
        if let Ok(s) = rig.sessions.try_recv() {
            break s;
        }
        assert!(t < 1000, "no session connected");
    };
    // Two failures grew the backoff to 2 s; 11 s connected resets it.
    for _ in 0..275 {
        t += 1;
        rig.say(chunk(t)).await;
    }
    let failed_at_tag = t;
    let failed_at = Instant::now();
    first
        .output
        .try_send(Output::Error(EngineError::Upstream("quota".to_owned())))
        .expect("engine output open");
    let mut second = loop {
        t += 1;
        rig.say(chunk(t)).await;
        if let Ok(s) = rig.sessions.try_recv() {
            break s;
        }
        assert!(t < 2000, "no reconnect");
    };
    assert!(
        near(second.at - failed_at, Duration::from_millis(500)),
        "{:?}",
        second.at - failed_at
    );
    sleep(Duration::from_millis(40)).await;
    let replay = second.drain();
    assert!(matches!(replay.first(), Some(In::A(t)) if *t < failed_at_tag - 100));
    assert_eq!(
        rig.events()[3..],
        [
            "start Continuous",
            "end Error Some(Upstream)",
            "start Continuous"
        ]
    );
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn changed_params_reconnect_a_long_session_once() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), false));
    rig.mic().await;
    let mut first = rig.session().await;
    assert!(first.params.hotwords.is_empty());
    for t in 1..=10 {
        rig.say(chunk(t)).await;
    }
    *rig.hotwords.lock().expect("lock") = vec!["Anya".to_owned()];
    rig.params.send_modify(|n| *n += 1);
    for t in 11..=15 {
        rig.say(chunk(t)).await;
    }
    let mut want = audio(1..=10);
    want.push(In::Finish);
    assert_eq!(first.until_finish().await, want);
    first.reply("first", true);
    let mut second = rig.session().await;
    assert_eq!(second.params.hotwords, ["Anya"]);
    for t in 16..=20 {
        rig.say(chunk(t)).await;
    }
    // Audio captured while reconnecting goes to the new session, nothing twice.
    assert_eq!(second.drain(), audio(11..=20));
    for t in 21..=150 {
        rig.say(chunk(t)).await;
    }
    rig.no_session();
    assert_eq!(
        rig.events()[3..],
        [
            "start Continuous",
            "result first final",
            "end Final None",
            "start Continuous"
        ]
    );
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn changed_params_leave_a_gated_utterance_alone() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), true));
    rig.mic().await;
    for t in 1001..=1003 {
        rig.say(chunk(t)).await;
    }
    let mut s = rig.session().await;
    rig.params.send_modify(|n| *n += 1);
    for t in 1004..=1006 {
        rig.say(chunk(t)).await;
    }
    for t in 1..=30 {
        rig.say(chunk(t)).await;
    }
    let mut want = audio(1001..=1006);
    want.extend(audio(1..=30));
    want.push(In::Finish);
    assert_eq!(s.until_finish().await, want);
    s.reply("one", true);
    sleep(Duration::from_millis(100)).await;
    rig.no_session();
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn switching_off_releases_the_device_and_finishes_the_session() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("loop-1"), false));
    let mic = rig.mic().await;
    let mut s = rig.session().await;
    for t in 1..=5 {
        rig.say(chunk(t)).await;
    }
    rig.continuous.set(false, settings(Some("loop-1"), false));
    timeout(Duration::from_millis(100), mic.tx.closed())
        .await
        .expect("device released at once");
    let mut want = audio(1..=5);
    want.push(In::Finish);
    assert_eq!(s.until_finish().await, want);
    s.reply("bye", true);
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        rig.hub.sources(),
        [(
            Source::Continuous,
            SourceState::Idle,
            Some("BlackHole 2ch".to_owned())
        )]
    );
    assert_eq!(
        rig.events(),
        [
            "hello",
            "Continuous Idle -",
            "Continuous Listening BlackHole 2ch",
            "start Continuous",
            "Continuous Idle BlackHole 2ch",
            "result bye final",
            "end Final None",
        ]
    );
    sleep(Duration::from_secs(10)).await;
    assert!(rig.mics.try_recv().is_err());
    rig.no_session();
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn rapid_switching_never_holds_two_devices() {
    let mut rig = Rig::new(true, 0, 0);
    let on = settings(Some("mic-1"), true);
    for _ in 0..5 {
        rig.continuous.set(true, on.clone());
        rig.continuous.set(false, on.clone());
    }
    for _ in 0..5 {
        rig.continuous.set(true, on.clone());
        tokio::task::yield_now().await;
        sleep(Duration::from_millis(1)).await;
        rig.continuous.set(false, on.clone());
        tokio::task::yield_now().await;
    }
    rig.continuous.set(true, on.clone());
    sleep(Duration::from_secs(1)).await;
    let mut mics = Vec::new();
    while let Ok(m) = rig.mics.try_recv() {
        mics.push(m);
    }
    let (last, earlier) = mics.split_last().expect("a device is open");
    assert!(earlier.iter().all(|m| m.tx.is_closed()));
    assert!(!last.tx.is_closed());
    assert!(!rig.audio.overlapped.load(Ordering::SeqCst));
    assert_eq!(
        rig.hub.sources(),
        [(
            Source::Continuous,
            SourceState::Listening,
            Some("Desk Mic".to_owned())
        )]
    );
    rig.mic = Some(last.tx.clone());
    for t in 1001..=1003 {
        rig.say(chunk(t)).await;
    }
    rig.session().await;
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn only_another_device_reopens_capture() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), true));
    let first = rig.mic().await;
    rig.continuous.set(true, settings(Some("mic-1"), true));
    let mut slower = settings(Some("mic-1"), true);
    slower.final_timeout = Duration::from_secs(3);
    rig.continuous.set(true, slower);
    sleep(Duration::from_secs(1)).await;
    assert!(rig.mics.try_recv().is_err());
    assert!(!first.tx.is_closed());

    rig.continuous.set(true, settings(Some("loop-1"), true));
    let second = rig.mic().await;
    assert_eq!(second.device.as_deref(), Some("loop-1"));
    assert!(first.tx.is_closed());
    assert_eq!(
        rig.events(),
        [
            "hello",
            "Continuous Idle -",
            "Continuous Listening Desk Mic",
            "Continuous Idle Desk Mic",
            "Continuous Listening BlackHole 2ch",
        ]
    );
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn failed_capture_goes_idle_and_reopens_after_a_backoff() {
    let mut rig = Rig::new(true, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), true));
    let first = rig.mic().await;
    first
        .tx
        .send(Err(AudioError::Backend("unplugged".to_owned())))
        .await
        .expect("capture open");
    let failed_at = Instant::now();
    let second = rig.mic().await;
    assert!(near(second.at - failed_at, Duration::from_millis(500)));
    assert_eq!(
        rig.events(),
        [
            "hello",
            "Continuous Idle -",
            "Continuous Listening Desk Mic",
            "Continuous Idle Desk Mic",
            "Continuous Listening Desk Mic",
        ]
    );
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn a_device_that_will_not_open_is_retried_with_growing_backoff() {
    let mut rig = Rig::new(true, 0, 2);
    let start = Instant::now();
    rig.continuous.set(true, settings(Some("mic-1"), true));
    let mic = rig.mic().await;
    assert!(near(mic.at - start, Duration::from_millis(1500)));
    assert_eq!(
        rig.events(),
        [
            "hello",
            "Continuous Idle -",
            "Continuous Idle Desk Mic",
            "Continuous Listening Desk Mic",
        ]
    );
    rig.check();
}

#[tokio::test(start_paused = true)]
async fn a_batch_engine_waits_for_the_gate() {
    let mut rig = Rig::new(false, 0, 0);
    rig.continuous.set(true, settings(Some("mic-1"), false));
    rig.mic().await;
    for t in 1001..=1050 {
        rig.say(chunk(t)).await;
    }
    rig.no_session();
    assert!(rig.engine.attempts.lock().expect("lock").is_empty());

    rig.continuous.set(true, settings(Some("mic-1"), true));
    // The new settings reach capture before the next chunk.
    sleep(Duration::from_millis(1)).await;
    for t in 1..=5 {
        rig.say(chunk(t)).await;
    }
    for t in 1001..=1003 {
        rig.say(chunk(t)).await;
    }
    for t in 6..=35 {
        rig.say(chunk(t)).await;
    }
    let mut s = rig.session().await;
    let mut want = audio(1..=5);
    want.extend(audio(1001..=1003));
    want.extend(audio(6..=35));
    want.push(In::Finish);
    assert_eq!(s.until_finish().await, want);
    s.reply("batch", true);
    sleep(Duration::from_millis(100)).await;
    assert!(rig.mics.try_recv().is_err());
    rig.check();
}
