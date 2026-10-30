//! A WAV file played as if it were an input device.

use std::io::Read;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anya_core::audio::{AudioError, AudioSource, CHUNK_MS, Capture, Chunk, Device, DeviceKind};
use tokio::sync::mpsc;

use crate::convert::Converter;
use crate::select;

/// Chunks buffered ahead of the reader.
const QUEUE: usize = 64;

/// Frames decoded per step.
const FRAMES: usize = 1024;

/// Plays a WAV file (any rate, channel count, integer or float samples) as
/// 16 kHz mono chunks, then ends the capture. For demos and smoke tests
/// without a microphone.
///
/// It lists one device, the file: id is the path, name the file name. The
/// file is the default of either kind, so it stands in for a microphone and
/// for system sound alike.
#[derive(Debug, Clone)]
pub struct Replay {
    path: PathBuf,
    realtime: bool,
}

impl Replay {
    /// Replays `path`. With `realtime` each chunk arrives when a device
    /// would have delivered it (one per 40 ms); without, as fast as the
    /// reader takes them.
    pub fn new(path: impl Into<PathBuf>, realtime: bool) -> Replay {
        Replay {
            path: path.into(),
            realtime,
        }
    }

    fn id(&self) -> String {
        self.path.display().to_string()
    }

    fn device(&self) -> Device {
        Device {
            id: self.id(),
            name: self
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.id()),
            kind: DeviceKind::Microphone,
        }
    }
}

impl AudioSource for Replay {
    fn devices(&self) -> Result<Vec<Device>, AudioError> {
        Ok(if self.path.is_file() {
            vec![self.device()]
        } else {
            Vec::new()
        })
    }

    fn open(&self, device: Option<&str>, _default: DeviceKind) -> Result<Capture, AudioError> {
        if let Some(wanted) = device {
            select::find(&self.devices()?, wanted)?;
        }
        let reader = hound::WavReader::open(&self.path).map_err(|e| match e {
            hound::Error::IoError(io) if io.kind() == std::io::ErrorKind::NotFound => {
                select::not_found(&self.id(), &[])
            }
            other => AudioError::Backend(format!("cannot read {}: {other}", self.id())),
        })?;
        let (tx, rx) = mpsc::channel(QUEUE);
        let realtime = self.realtime;
        thread::Builder::new()
            .name("anya-replay".into())
            .spawn(move || play(reader, realtime, tx))
            .map_err(|e| AudioError::Backend(format!("cannot start the replay thread: {e}")))?;
        Ok(Capture { chunks: rx })
    }
}

fn play<R: Read>(
    reader: hound::WavReader<R>,
    realtime: bool,
    tx: mpsc::Sender<Result<Chunk, AudioError>>,
) {
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let mut converter = Converter::new(spec.sample_rate, spec.channels, 1);
    let mut pacer = Pacer {
        tx,
        realtime,
        start: Instant::now(),
        sent: 0,
        closed: false,
    };
    let result = match spec.sample_format {
        hound::SampleFormat::Float => feed(
            reader.into_samples::<f32>(),
            channels,
            |s| s,
            &mut converter,
            &mut pacer,
        ),
        hound::SampleFormat::Int => {
            let bits = u32::from(spec.bits_per_sample).clamp(1, 32);
            let scale = 1.0 / (1u64 << (bits - 1)) as f32;
            feed(
                reader.into_samples::<i32>(),
                channels,
                move |s: i32| s as f32 * scale,
                &mut converter,
                &mut pacer,
            )
        }
    };
    if pacer.closed {
        return;
    }
    converter.finish(|chunk| pacer.send(chunk));
    if let Err(e) = result {
        pacer.fail(AudioError::Backend(format!(
            "cannot decode the WAV file: {e}"
        )));
    }
}

/// Decodes `samples` block by block into `pacer`. Stops early when the
/// reader is gone; an error comes back after what decoded before it.
fn feed<S: Copy>(
    mut samples: impl Iterator<Item = Result<S, hound::Error>>,
    channels: usize,
    to_f32: impl Fn(S) -> f32,
    converter: &mut Converter,
    pacer: &mut Pacer,
) -> Result<(), hound::Error> {
    let block = channels * FRAMES;
    let mut buf = Vec::with_capacity(block);
    loop {
        buf.clear();
        let mut error = None;
        for sample in samples.by_ref().take(block) {
            match sample {
                Ok(s) => buf.push(s),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        converter.push(&buf, &to_f32, |chunk| pacer.send(chunk));
        if let Some(e) = error {
            return Err(e);
        }
        if buf.len() < block || pacer.closed {
            return Ok(());
        }
    }
}

/// Hands chunks to the reader, at device pace when `realtime`.
struct Pacer {
    tx: mpsc::Sender<Result<Chunk, AudioError>>,
    realtime: bool,
    start: Instant,
    sent: u32,
    closed: bool,
}

impl Pacer {
    fn send(&mut self, chunk: Chunk) {
        if self.closed {
            return;
        }
        self.sent += 1;
        if self.realtime {
            // A device delivers chunk n once its 40 ms have been recorded.
            let due = self.start + Duration::from_millis(u64::from(CHUNK_MS)) * self.sent;
            let now = Instant::now();
            if due > now {
                thread::sleep(due - now);
            }
        }
        if self.tx.blocking_send(Ok(chunk)).is_err() {
            self.closed = true;
        }
    }

    fn fail(&mut self, error: AudioError) {
        if !self.closed {
            let _ = self.tx.blocking_send(Err(error));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anya_core::audio::{CHUNK_BYTES, SAMPLE_RATE};

    use super::*;

    struct TempWav(PathBuf);

    impl Drop for TempWav {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn wav(
        name: &str,
        spec: hound::WavSpec,
        frames: usize,
        sample: impl Fn(usize) -> f64,
    ) -> TempWav {
        let path =
            std::env::temp_dir().join(format!("anya-audio-{}-{name}.wav", std::process::id()));
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..frames {
            let x = sample(i);
            for _ in 0..spec.channels {
                match spec.sample_format {
                    hound::SampleFormat::Float => writer.write_sample(x as f32).unwrap(),
                    hound::SampleFormat::Int => {
                        let full = f64::from(1u32 << (spec.bits_per_sample - 1));
                        let v = (x * full).round().clamp(-full, full - 1.0) as i32;
                        writer.write_sample(v).unwrap();
                    }
                }
            }
        }
        writer.finalize().unwrap();
        TempWav(path)
    }

    fn spec(rate: u32, channels: u16, bits: u16, format: hound::SampleFormat) -> hound::WavSpec {
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: bits,
            sample_format: format,
        }
    }

    fn collect(path: &Path, realtime: bool) -> Vec<Chunk> {
        let mut chunks = Replay::new(path, realtime)
            .open(None, DeviceKind::Microphone)
            .unwrap()
            .chunks;
        let mut out = Vec::new();
        while let Some(chunk) = chunks.blocking_recv() {
            out.push(chunk.unwrap());
        }
        out
    }

    fn tone(rate: u32) -> impl Fn(usize) -> f64 {
        move |i| 0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin()
    }

    #[test]
    fn a_second_of_any_format_is_25_exact_chunks() {
        let int = hound::SampleFormat::Int;
        let float = hound::SampleFormat::Float;
        let cases = [
            ("s16-44k-stereo", spec(44_100, 2, 16, int)),
            ("s8-8k", spec(8_000, 1, 8, int)),
            ("s24-48k", spec(48_000, 1, 24, int)),
            ("s32-96k-4ch", spec(96_000, 4, 32, int)),
            ("f32-48k-stereo", spec(48_000, 2, 32, float)),
        ];
        for (name, spec) in cases {
            let file = wav(
                name,
                spec,
                spec.sample_rate as usize,
                tone(spec.sample_rate),
            );
            let chunks = collect(&file.0, false);
            assert_eq!(chunks.len(), 25, "{name}");
            assert!(chunks.iter().all(|c| c.0.len() == CHUNK_BYTES), "{name}");
            // The tone survives: a 0.5 sine is about -9 dBFS RMS.
            assert!(
                chunks[12].level() > 0.8,
                "{name}: level {}",
                chunks[12].level()
            );
        }
    }

    #[test]
    fn a_partial_last_chunk_is_padded_with_silence() {
        let rate = SAMPLE_RATE as usize;
        let file = wav(
            "partial",
            spec(SAMPLE_RATE, 1, 16, hound::SampleFormat::Int),
            rate + 160,
            |_| 0.25,
        );
        let chunks = collect(&file.0, false);
        assert_eq!(chunks.len(), 26);
        let last = &chunks[25].0;
        assert_eq!(last.len(), CHUNK_BYTES);
        assert_eq!(i16::from_le_bytes([last[0], last[1]]), 8192);
        assert!(last[320..].iter().all(|&b| b == 0));
    }

    #[test]
    fn s16_mono_16khz_replays_byte_for_byte() {
        let file = wav(
            "exact",
            spec(SAMPLE_RATE, 1, 16, hound::SampleFormat::Int),
            1280,
            |i| ((i as f64 * 0.37).sin()) * 0.9,
        );
        let mut reader = hound::WavReader::open(&file.0).unwrap();
        let expected: Vec<u8> = reader
            .samples::<i16>()
            .flat_map(|s| s.unwrap().to_le_bytes())
            .collect();
        let bytes: Vec<u8> = collect(&file.0, false)
            .into_iter()
            .flat_map(|c| c.0)
            .collect();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn realtime_replay_keeps_device_pace() {
        // 0.2 s: five chunks, the last due 200 ms after opening.
        let file = wav(
            "paced",
            spec(SAMPLE_RATE, 1, 16, hound::SampleFormat::Int),
            3200,
            |_| 0.1,
        );
        let started = Instant::now();
        assert_eq!(collect(&file.0, true).len(), 5);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(1000), "{elapsed:?}");

        let started = Instant::now();
        assert_eq!(collect(&file.0, false).len(), 5);
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn the_file_is_the_only_device() {
        let file = wav(
            "listed",
            spec(SAMPLE_RATE, 1, 16, hound::SampleFormat::Int),
            640,
            |_| 0.0,
        );
        let replay = Replay::new(&file.0, false);
        let devices = replay.devices().unwrap();
        assert_eq!(devices.len(), 1);
        let name = devices[0].name.clone();
        assert!(name.ends_with("-listed.wav"));
        let mic = DeviceKind::Microphone;
        assert!(replay.open(Some(&devices[0].id), mic).is_ok());
        assert!(replay.open(Some(&name.to_uppercase()), mic).is_ok());
        assert!(replay.open(None, DeviceKind::Loopback).is_ok());
        match replay.open(Some("microphone"), mic) {
            Err(AudioError::NotFound { wanted, available }) => {
                assert_eq!(wanted, "microphone");
                assert_eq!(available, [name]);
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_file_is_not_found() {
        let replay = Replay::new("/nonexistent/anya.wav", false);
        assert!(replay.devices().unwrap().is_empty());
        assert!(matches!(
            replay.open(None, DeviceKind::Microphone),
            Err(AudioError::NotFound { .. })
        ));
        assert!(matches!(
            replay.open(Some("anya.wav"), DeviceKind::Microphone),
            Err(AudioError::NotFound { available, .. }) if available.is_empty()
        ));
    }
}
