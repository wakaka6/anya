//! Capture: the one audio format inside Anya, and the seam devices are opened through.

use tokio::sync::mpsc;

/// Sample rate of every [`Chunk`].
pub const SAMPLE_RATE: u32 = 16_000;

/// How much audio an adapter puts in one chunk: 40 ms, so a level meter
/// moves about 25 times a second.
pub const CHUNK_MS: u32 = 40;

/// Bytes in a [`CHUNK_MS`] chunk.
pub const CHUNK_BYTES: usize = (SAMPLE_RATE * CHUNK_MS / 1000 * 2) as usize;

/// Captured audio: signed 16-bit little-endian mono PCM at [`SAMPLE_RATE`].
///
/// This is the only audio format that crosses a seam. Adapters convert before
/// sending; an engine that needs less resamples itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk(pub Vec<u8>);

impl Chunk {
    /// Loudness for a level meter: RMS from -60 dBFS (0.0) to full scale (1.0).
    pub fn level(&self) -> f32 {
        let samples = self
            .0
            .chunks_exact(2)
            .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])));
        let (sum, n) = samples.fold((0.0, 0u32), |(s, n), x| (s + x * x, n + 1));
        if n == 0 || sum == 0.0 {
            return 0.0;
        }
        let rms = (sum / f64::from(n)).sqrt() / 32768.0;
        let db = 20.0 * rms.log10();
        ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f32
    }
}

/// Wraps PCM in a WAV header, for engines that take a file.
pub fn wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let len = u32::try_from(pcm.len()).unwrap_or(u32::MAX - 36);
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// What a device records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// The outside world: the user's voice, the room.
    Microphone,
    /// What this computer plays: meetings, videos.
    Loopback,
}

/// An input device as an [`AudioSource`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Stable id to open it by and keep in the config.
    pub id: String,
    /// Name to show.
    pub name: String,
    /// What it records.
    pub kind: DeviceKind,
}

/// Why capture could not start, or stopped. UIs translate the variant; the
/// Display text is for logs.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// The requested device is not connected. Answered with this error, never
    /// by opening another device.
    #[error("input device {wanted:?} not found; available: {}", .available.join(", "))]
    NotFound {
        /// The id or name asked for.
        wanted: String,
        /// Names of the devices that are connected.
        available: Vec<String>,
    },
    /// The system refused to let Anya record.
    #[error("permission to record was denied: {0}")]
    Denied(String),
    /// The platform's audio system failed.
    #[error("audio capture failed: {0}")]
    Backend(String),
}

/// A running capture. Dropping it stops capture and releases the device.
#[derive(Debug)]
pub struct Capture {
    /// Chunks in order. An `Err` is the last item: capture died and the device
    /// is released.
    pub chunks: mpsc::Receiver<Result<Chunk, AudioError>>,
}

/// Lists and opens input devices. Adapters live in `anya-audio`.
pub trait AudioSource: Send + Sync {
    /// The devices connected now.
    fn devices(&self) -> Result<Vec<Device>, AudioError>;

    /// Starts capturing `device` (an id from [`AudioSource::devices`], or
    /// `None` for the system default). Returns once the device is open.
    fn open(&self, device: Option<&str>) -> Result<Capture, AudioError>;
}
