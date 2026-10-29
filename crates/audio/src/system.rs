//! Capture from the machine's input devices through cpal.
//!
//! Each capture owns one thread: it opens the device, keeps the stream, and
//! ends it when the consumer drops the receiver or the stream fails. The
//! audio callback only converts and `try_send`s; it never blocks.

use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use anya_core::audio::{AudioError, AudioSource, Capture, Chunk, Device};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::classify;
use crate::convert::Converter;
use crate::select;

/// Chunks a capture buffers for a slow reader: about 20 s.
const QUEUE: usize = 512;

/// How often the capture thread checks whether the reader is gone.
const POLL: Duration = Duration::from_millis(50);

type Sender = mpsc::Sender<Result<Chunk, AudioError>>;

/// The machine's input devices: ALSA on Linux, CoreAudio on macOS.
///
/// Device ids are ALSA PCM names on Linux and CoreAudio UIDs on macOS.
#[derive(Debug, Clone)]
pub struct System {
    gain: i32,
}

impl System {
    /// A source whose chunks are multiplied by `gain`, saturating at full
    /// scale. A gain below 1 (an unset 0 included) counts as 1.
    pub fn new(gain: i32) -> System {
        System { gain: gain.max(1) }
    }
}

impl AudioSource for System {
    fn devices(&self) -> Result<Vec<Device>, AudioError> {
        list(&cpal::default_host()).map(|(devices, _)| devices)
    }

    fn open(&self, device: Option<&str>) -> Result<Capture, AudioError> {
        #[cfg(target_os = "macos")]
        crate::macos::authorize()?;
        let (tx, rx) = mpsc::channel(QUEUE);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let wanted = device.map(str::to_owned);
        let gain = self.gain;
        thread::Builder::new()
            .name("anya-capture".into())
            .spawn(move || run(wanted, gain, tx, ready_tx))
            .map_err(|e| AudioError::Backend(format!("cannot start the capture thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Capture { chunks: rx }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioError::Backend(
                "the capture thread ended before the device opened".into(),
            )),
        }
    }
}

/// What the stream tells its capture thread.
enum Event {
    /// cpal reported an error.
    Failed(cpal::Error),
    /// The reader fell [`QUEUE`] chunks behind.
    Overflow,
    /// The reader dropped the receiver.
    Closed,
}

fn run(
    wanted: Option<String>,
    gain: i32,
    tx: Sender,
    ready: std_mpsc::SyncSender<Result<(), AudioError>>,
) {
    let host = cpal::default_host();
    let (events_tx, events) = std_mpsc::channel();
    let (stream, label) = match start(&host, wanted.as_deref(), gain, tx.clone(), events_tx) {
        Ok(started) => started,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let failure = loop {
        match events.recv_timeout(POLL) {
            Ok(Event::Failed(err)) if fatal(err.kind()) => {
                break Some(audio_error(&host, &label, err));
            }
            Ok(Event::Failed(err)) => tracing::warn!(device = %label, %err, "audio glitch"),
            Ok(Event::Overflow) => {
                // Dropping audio in the middle would corrupt the transcript;
                // ending is the honest answer.
                break Some(AudioError::Backend(
                    "the reader fell behind; capture stopped instead of dropping audio".into(),
                ));
            }
            Ok(Event::Closed) => break None,
            Err(std_mpsc::RecvTimeoutError::Timeout) if tx.is_closed() => break None,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                break Some(AudioError::Backend(
                    "the audio stream ended unexpectedly".into(),
                ));
            }
        }
    };
    drop(stream);
    match failure {
        Some(error) => {
            tracing::warn!(device = %label, %error, "recording failed");
            let _ = tx.blocking_send(Err(error));
        }
        None => tracing::info!(device = %label, "recording stopped"),
    }
}

/// Opens the device and starts its stream. Returns the stream and the label
/// errors name the device by.
fn start(
    host: &cpal::Host,
    wanted: Option<&str>,
    gain: i32,
    tx: Sender,
    events: std_mpsc::Sender<Event>,
) -> Result<(cpal::Stream, String), AudioError> {
    let (device, label) = match wanted {
        None => {
            let device = host
                .default_input_device()
                .ok_or_else(|| select::not_found("default", &listed(host)))?;
            let label = device
                .description()
                .map(|d| d.name().to_owned())
                .unwrap_or_else(|_| "default".into());
            (device, label)
        }
        Some(wanted) => {
            let (devices, mut handles) = list(host)?;
            let i = select::find(&devices, wanted)?;
            tracing::debug!(id = %devices[i].id, name = %devices[i].name, kind = ?devices[i].kind, "audio input device selected");
            (handles.swap_remove(i), wanted.trim().to_owned())
        }
    };
    let config = device
        .default_input_config()
        .map_err(|e| audio_error(host, &label, e))?;
    let converter = Converter::new(config.sample_rate(), config.channels(), gain);
    let stream_config = config.config();
    let device_ref = &device;
    let stream = match config.sample_format() {
        SampleFormat::I8 => build::<i8>(device_ref, stream_config, converter, tx, events),
        SampleFormat::I16 => build::<i16>(device_ref, stream_config, converter, tx, events),
        SampleFormat::I24 => build::<cpal::I24>(device_ref, stream_config, converter, tx, events),
        SampleFormat::I32 => build::<i32>(device_ref, stream_config, converter, tx, events),
        SampleFormat::I64 => build::<i64>(device_ref, stream_config, converter, tx, events),
        SampleFormat::U8 => build::<u8>(device_ref, stream_config, converter, tx, events),
        SampleFormat::U16 => build::<u16>(device_ref, stream_config, converter, tx, events),
        SampleFormat::U24 => build::<cpal::U24>(device_ref, stream_config, converter, tx, events),
        SampleFormat::U32 => build::<u32>(device_ref, stream_config, converter, tx, events),
        SampleFormat::U64 => build::<u64>(device_ref, stream_config, converter, tx, events),
        SampleFormat::F32 => build::<f32>(device_ref, stream_config, converter, tx, events),
        SampleFormat::F64 => build::<f64>(device_ref, stream_config, converter, tx, events),
        other => {
            return Err(AudioError::Backend(format!(
                "{label}: unsupported sample format {other:?}"
            )));
        }
    }
    .map_err(|e| audio_error(host, &label, e))?;
    stream.play().map_err(|e| audio_error(host, &label, e))?;
    tracing::info!(
        device = %label,
        rate = config.sample_rate(),
        channels = config.channels(),
        format = ?config.sample_format(),
        "recording started"
    );
    Ok((stream, label))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut converter: Converter,
    tx: Sender,
    events: std_mpsc::Sender<Event>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let errors = events.clone();
    let mut stopped = false;
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if stopped {
                return;
            }
            converter.push(data, <f32 as FromSample<T>>::from_sample_, |chunk| {
                if stopped {
                    return;
                }
                match tx.try_send(Ok(chunk)) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        stopped = true;
                        let _ = events.send(Event::Overflow);
                    }
                    Err(TrySendError::Closed(_)) => {
                        stopped = true;
                        let _ = events.send(Event::Closed);
                    }
                }
            });
        },
        move |err| {
            let _ = errors.send(Event::Failed(err));
        },
        None,
    )
}

/// Whether a stream error ends the capture. Glitches, a reroute of the
/// default device and a refused real-time priority do not.
fn fatal(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Xrun
            | ErrorKind::DeviceChanged
            | ErrorKind::RealtimeDenied
            | ErrorKind::DeviceBusy
    )
}

fn audio_error(host: &cpal::Host, label: &str, err: cpal::Error) -> AudioError {
    match err.kind() {
        ErrorKind::PermissionDenied => AudioError::Denied(err.to_string()),
        ErrorKind::DeviceNotAvailable => select::not_found(label, &listed(host)),
        _ => AudioError::Backend(format!("{label}: {err}")),
    }
}

/// The device list for an error message; empty when it cannot be read.
fn listed(host: &cpal::Host) -> Vec<Device> {
    list(host).map(|(devices, _)| devices).unwrap_or_default()
}

fn backend(err: cpal::Error) -> AudioError {
    match err.kind() {
        ErrorKind::PermissionDenied => AudioError::Denied(err.to_string()),
        _ => AudioError::Backend(err.to_string()),
    }
}

/// The input devices worth listing, with their cpal handles in the same order.
#[cfg(target_os = "linux")]
fn list(host: &cpal::Host) -> Result<(Vec<Device>, Vec<cpal::Device>), AudioError> {
    let mut pcms = Vec::new();
    for device in host.input_devices().map_err(backend)? {
        let Ok(id) = device.id() else { continue };
        let id = id.id().to_owned();
        let (title, description) = match device.description() {
            Ok(desc) => (
                desc.name().trim().to_owned(),
                desc.extended().collect::<Vec<_>>().join("\n"),
            ),
            Err(_) => (id.clone(), String::new()),
        };
        let title = if title.is_empty() { id.clone() } else { title };
        pcms.push((id, title, description, device));
    }
    let keys: Vec<(&str, &str)> = pcms
        .iter()
        .map(|(id, title, _, _)| (id.as_str(), title.as_str()))
        .collect();
    let keep = classify::alsa_listed(&keys);
    let mut entries = Vec::new();
    let mut handles = Vec::new();
    for ((id, title, description, device), keep) in pcms.into_iter().zip(keep) {
        if keep {
            let kind = classify::alsa_kind(&id, &description);
            entries.push((id, title, kind));
            handles.push(device);
        }
    }
    Ok((classify::named(entries), handles))
}

/// The input devices, with their cpal handles in the same order.
#[cfg(target_os = "macos")]
fn list(host: &cpal::Host) -> Result<(Vec<Device>, Vec<cpal::Device>), AudioError> {
    use anya_core::audio::DeviceKind;

    let kinds = crate::macos::device_kinds();
    let mut entries = Vec::new();
    let mut handles = Vec::new();
    for device in host.input_devices().map_err(backend)? {
        let Ok(id) = device.id() else { continue };
        let id = id.id().to_owned();
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| id.clone());
        let kind = kinds.get(&id).copied().unwrap_or(DeviceKind::Microphone);
        entries.push((id, name, kind));
        handles.push(device);
    }
    Ok((classify::named(entries), handles))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glitches_and_reroutes_keep_capturing() {
        for kind in [
            ErrorKind::Xrun,
            ErrorKind::DeviceChanged,
            ErrorKind::RealtimeDenied,
            ErrorKind::DeviceBusy,
        ] {
            assert!(!fatal(kind), "{kind:?}");
        }
    }

    #[test]
    fn a_vanished_device_or_a_broken_stream_ends_capture() {
        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::PermissionDenied,
            ErrorKind::StreamInvalidated,
            ErrorKind::BackendError,
            ErrorKind::Other,
        ] {
            assert!(fatal(kind), "{kind:?}");
        }
    }

    #[test]
    fn gain_below_one_counts_as_one() {
        assert_eq!(System::new(0).gain, 1);
        assert_eq!(System::new(-3).gain, 1);
        assert_eq!(System::new(4).gain, 4);
    }

    /// Needs a microphone: `cargo test -p anya-audio -- --ignored`.
    #[test]
    #[ignore]
    fn records_a_second_from_the_default_input() {
        let capture = System::new(1).open(None).expect("open default input");
        let mut chunks = capture.chunks;
        for _ in 0..25 {
            let chunk = chunks.blocking_recv().expect("stream open").expect("chunk");
            assert_eq!(chunk.0.len(), anya_core::audio::CHUNK_BYTES);
        }
    }
}
