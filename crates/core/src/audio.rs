//! Capture: the one audio format inside Anya, and the seam devices are opened through.

use tokio::sync::mpsc;

/// Sample rate of every [`Chunk`].
pub const SAMPLE_RATE: u32 = 16_000;

/// Captured audio: signed 16-bit little-endian mono PCM at [`SAMPLE_RATE`].
///
/// This is the only audio format that crosses a seam. Adapters convert before
/// sending; an engine that needs less resamples itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk(pub Vec<u8>);

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

/// Why capture could not start, or stopped.
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
