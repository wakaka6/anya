//! The service's binary WebSocket messages.
//!
//! Every message is a 4-byte header, an optional sequence number, the
//! payload's size and the payload; integers are big-endian. Header byte 0 is
//! the protocol version (1) and the header size in 4-byte words (1); byte 1
//! the message type and flags; byte 2 the serialization and compression;
//! byte 3 is reserved. Error messages carry an error code and a message in
//! place of the sequence and payload.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

/// The client's first message: the session's parameters as JSON.
pub(crate) const FULL_CLIENT_REQUEST: u8 = 0b0001;
/// A packet of audio.
pub(crate) const AUDIO_ONLY: u8 = 0b0010;
/// A recognition result as JSON.
pub(crate) const FULL_SERVER_RESPONSE: u8 = 0b1001;
/// The service failed; the session is over.
pub(crate) const ERROR: u8 = 0b1111;

const VERSION_AND_HEADER_SIZE: u8 = 0x11;
const FLAG_SEQUENCE: u8 = 0b0001;
const FLAG_LAST: u8 = 0b0010;
const SERIALIZATION_JSON: u8 = 0b0001;
const COMPRESSION_GZIP: u8 = 0b0001;

/// One message other than an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Packet {
    /// Message type: [`FULL_CLIENT_REQUEST`], [`AUDIO_ONLY`],
    /// [`FULL_SERVER_RESPONSE`] or one this crate does not know.
    pub(crate) message: u8,
    /// The sequence number, when the message carries one.
    pub(crate) sequence: Option<i32>,
    /// The last message of its direction.
    pub(crate) last: bool,
    /// The payload is JSON; otherwise raw bytes.
    pub(crate) json: bool,
    /// The payload travels gzip-compressed.
    pub(crate) gzip: bool,
    /// The payload, uncompressed.
    pub(crate) payload: Vec<u8>,
}

impl Packet {
    /// The session's parameters, sent uncompressed and without a sequence.
    pub(crate) fn full_client_request(json: Vec<u8>) -> Packet {
        Packet {
            message: FULL_CLIENT_REQUEST,
            sequence: None,
            last: false,
            json: true,
            gzip: false,
            payload: json,
        }
    }

    /// Raw PCM, uncompressed and without a sequence; `last` ends the audio.
    pub(crate) fn audio(pcm: Vec<u8>, last: bool) -> Packet {
        Packet {
            message: AUDIO_ONLY,
            sequence: None,
            last,
            json: false,
            gzip: false,
            payload: pcm,
        }
    }
}

/// A decoded message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Message {
    /// Anything but an error.
    Packet(Packet),
    /// The service's error code and its own words.
    Error {
        /// The service's code, e.g. `45000001`.
        code: u32,
        /// The service's message, untranslated.
        message: String,
    },
}

/// Why bytes are not a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FrameError {
    /// Shorter than its header.
    #[error("frame too short ({0} bytes)")]
    TooShort(usize),
    /// The flags announce a sequence number that is not there.
    #[error("frame is missing its sequence number")]
    MissingSequence,
    /// The payload size is not there.
    #[error("frame is missing its payload size")]
    MissingSize,
    /// The payload size runs past the end.
    #[error("frame payload size {size} exceeds the {remaining} bytes left")]
    BadSize {
        /// The size the frame states.
        size: u32,
        /// The bytes after the size field.
        remaining: usize,
    },
    /// The payload is flagged gzip but does not decompress.
    #[error("frame payload does not gunzip: {0}")]
    Gunzip(String),
}

impl Message {
    /// The bytes to send in one binary WebSocket message.
    pub(crate) fn encode(&self) -> Vec<u8> {
        match self {
            Message::Packet(p) => {
                let flags = if p.sequence.is_some() {
                    FLAG_SEQUENCE
                } else {
                    0
                } | if p.last { FLAG_LAST } else { 0 };
                let payload = if p.gzip {
                    gzip(&p.payload)
                } else {
                    p.payload.clone()
                };
                let mut out = header(p.message, flags, p.json, p.gzip);
                if let Some(seq) = p.sequence {
                    out.extend_from_slice(&seq.to_be_bytes());
                }
                push_sized(&mut out, &payload);
                out
            }
            Message::Error { code, message } => {
                let mut out = header(ERROR, 0, true, false);
                out.extend_from_slice(&code.to_be_bytes());
                push_sized(&mut out, message.as_bytes());
                out
            }
        }
    }

    /// Reads one binary WebSocket message, from either side.
    pub(crate) fn decode(raw: &[u8]) -> Result<Message, FrameError> {
        let header_len = usize::from(raw.first().map_or(0, |b| b & 0x0f)) * 4;
        if header_len < 4 || raw.len() < header_len {
            return Err(FrameError::TooShort(raw.len()));
        }
        let message = raw[1] >> 4;
        let flags = raw[1] & 0x0f;
        let json = raw[2] >> 4 == SERIALIZATION_JSON;
        let gzip = raw[2] & 0x0f == COMPRESSION_GZIP;
        let mut rest = &raw[header_len..];

        if message == ERROR {
            let code = take_u32(&mut rest).ok_or(FrameError::TooShort(raw.len()))?;
            let body = take_sized(&mut rest)?;
            // The words matter more than the encoding: keep them as sent if
            // they do not decompress.
            let body = if gzip {
                gunzip(body).unwrap_or_else(|_| body.to_vec())
            } else {
                body.to_vec()
            };
            return Ok(Message::Error {
                code,
                message: String::from_utf8_lossy(&body).into_owned(),
            });
        }

        let sequence = if flags & FLAG_SEQUENCE != 0 {
            let seq = take_u32(&mut rest).ok_or(FrameError::MissingSequence)?;
            Some(i32::from_be_bytes(seq.to_be_bytes()))
        } else {
            None
        };
        let body = take_sized(&mut rest)?;
        let payload = if gzip && !body.is_empty() {
            gunzip(body).map_err(|e| FrameError::Gunzip(e.to_string()))?
        } else {
            body.to_vec()
        };
        Ok(Message::Packet(Packet {
            message,
            sequence,
            last: flags & FLAG_LAST != 0,
            json,
            gzip,
            payload,
        }))
    }
}

fn header(message: u8, flags: u8, json: bool, gzip: bool) -> Vec<u8> {
    let serialization = if json { SERIALIZATION_JSON } else { 0 };
    let compression = if gzip { COMPRESSION_GZIP } else { 0 };
    vec![
        VERSION_AND_HEADER_SIZE,
        message << 4 | flags,
        serialization << 4 | compression,
        0,
    ]
}

fn push_sized(out: &mut Vec<u8>, payload: &[u8]) {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
}

fn take_u32(rest: &mut &[u8]) -> Option<u32> {
    let (head, tail) = rest.split_first_chunk::<4>()?;
    *rest = tail;
    Some(u32::from_be_bytes(*head))
}

fn take_sized<'a>(rest: &mut &'a [u8]) -> Result<&'a [u8], FrameError> {
    let size = take_u32(rest).ok_or(FrameError::MissingSize)?;
    let remaining = rest.len();
    usize::try_from(size)
        .ok()
        .and_then(|n| rest.get(..n))
        .ok_or(FrameError::BadSize { size, remaining })
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    // Writing into a Vec cannot fail.
    let _ = encoder.write_all(bytes);
    encoder.finish().unwrap_or_default()
}

fn gunzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    GzDecoder::new(bytes).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(sequence: Option<i32>, last: bool, gzip: bool, payload: &[u8]) -> Message {
        Message::Packet(Packet {
            message: FULL_SERVER_RESPONSE,
            sequence,
            last,
            json: true,
            gzip,
            payload: payload.to_vec(),
        })
    }

    #[test]
    fn client_packets_have_the_documented_headers() {
        let init = Message::Packet(Packet::full_client_request(b"{}".to_vec())).encode();
        assert_eq!(init, [0x11, 0x10, 0x10, 0x00, 0, 0, 0, 2, b'{', b'}']);
        let audio = Message::Packet(Packet::audio(vec![1, 2, 3], false)).encode();
        assert_eq!(audio, [0x11, 0x20, 0x00, 0x00, 0, 0, 0, 3, 1, 2, 3]);
        let last = Message::Packet(Packet::audio(Vec::new(), true)).encode();
        assert_eq!(last, [0x11, 0x22, 0x00, 0x00, 0, 0, 0, 0]);
    }

    #[test]
    fn packets_round_trip_with_sequence_and_flags() {
        for (sequence, last) in [
            (None, false),
            (Some(7), false),
            (None, true),
            (Some(-3), true),
        ] {
            let m = server(sequence, last, false, br#"{"result":{"text":"hi"}}"#);
            assert_eq!(Message::decode(&m.encode()), Ok(m));
        }
        let flags = |m: Message| m.encode()[1] & 0x0f;
        assert_eq!(flags(server(Some(1), false, false, b"")), 0b0001);
        assert_eq!(flags(server(Some(-1), true, false, b"")), 0b0011);
    }

    #[test]
    fn gzip_payloads_are_compressed_on_the_wire_and_decoded() {
        let text = r#"{"result":{"text":"你好你好你好你好你好你好"}}"#.as_bytes();
        let m = server(Some(1), false, true, text);
        let bytes = m.encode();
        assert_eq!(bytes[2], 0x11);
        assert_eq!(
            &bytes[12..14],
            [0x1f, 0x8b],
            "a gzip stream follows the size"
        );
        assert_eq!(Message::decode(&bytes), Ok(m));
    }

    #[test]
    fn error_frames_carry_the_code_and_message() {
        let m = Message::Error {
            code: 45_000_001,
            message: "invalid request".into(),
        };
        let bytes = m.encode();
        assert_eq!(bytes[1], 0xf0);
        assert_eq!(Message::decode(&bytes), Ok(m));

        let mut gzipped = header(ERROR, 0, true, true);
        gzipped.extend_from_slice(&55_000_031u32.to_be_bytes());
        push_sized(&mut gzipped, &gzip("server busy".as_bytes()));
        assert_eq!(
            Message::decode(&gzipped),
            Ok(Message::Error {
                code: 55_000_031,
                message: "server busy".into()
            })
        );
    }

    #[test]
    fn malformed_frames_are_errors() {
        assert_eq!(Message::decode(&[0x11, 0x90]), Err(FrameError::TooShort(2)));
        assert_eq!(
            Message::decode(&[0x11, 0x91, 0x10, 0x00]),
            Err(FrameError::MissingSequence)
        );
        assert_eq!(
            Message::decode(&[0x11, 0x90, 0x10, 0x00]),
            Err(FrameError::MissingSize)
        );
        assert_eq!(
            Message::decode(&[0x11, 0x90, 0x10, 0x00, 0, 0, 0, 9, 1]),
            Err(FrameError::BadSize {
                size: 9,
                remaining: 1
            })
        );
        assert!(matches!(
            Message::decode(&[0x11, 0x90, 0x11, 0x00, 0, 0, 0, 2, 1, 2]),
            Err(FrameError::Gunzip(_))
        ));
        assert_eq!(
            Message::decode(&[0x11, 0xf0, 0x10, 0x00, 0, 0, 0]),
            Err(FrameError::TooShort(7))
        );
    }
}
