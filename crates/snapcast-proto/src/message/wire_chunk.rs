//! Wire Chunk message (type=2).
//!
//! Sent from server to client. Contains a timestamp and encoded audio data.

use std::io::{Read, Write};

use crate::message::base::ProtoError;
use crate::message::wire;
use crate::types::Timeval;

/// Wire chunk payload — a timestamped piece of encoded audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireChunk {
    /// Raw server-side timestamp/time marker from the wire payload.
    pub timestamp: Timeval,
    /// Encoded audio payload.
    pub payload: Vec<u8>,
}

impl WireChunk {
    /// Wire size: timestamp (8) + u32 len + payload.
    pub fn wire_size(&self) -> u32 {
        8 + wire::bytes_wire_size(&self.payload)
    }

    /// Deserialize a wire chunk from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let timestamp = Timeval::read_from(r)?;
        let payload = wire::read_bytes(r)?;
        Ok(Self { timestamp, payload })
    }

    /// Serialize a wire chunk to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        self.timestamp.write_to(w)?;
        wire::write_bytes(w, &self.payload)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_MAX_PAYLOAD_SIZE;

    struct FailingWriter;

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("forced write failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn round_trip() {
        let original = WireChunk {
            timestamp: Timeval {
                sec: 1000,
                usec: 500_000,
            },
            payload: vec![0xAA, 0xBB, 0xCC, 0xDD],
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = WireChunk::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn known_bytes() {
        let expected: Vec<u8> = vec![
            0xE8, 0x03, 0x00, 0x00, // timestamp.sec = 1000
            0x00, 0x00, 0x00, 0x00, // timestamp.usec = 0
            0x02, 0x00, 0x00, 0x00, // payload len = 2
            0xAB, 0xCD, // payload
        ];
        let msg = WireChunk {
            timestamp: Timeval { sec: 1000, usec: 0 },
            payload: vec![0xAB, 0xCD],
        };
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        assert_eq!(buf, expected);
    }

    #[test]
    fn wire_size_matches_serialized_length() {
        let msg = WireChunk {
            timestamp: Timeval::default(),
            payload: vec![0; 960],
        };
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        assert_eq!(msg.wire_size(), buf.len() as u32);
    }

    #[test]
    fn empty_payload_round_trip() {
        let original = WireChunk {
            timestamp: Timeval { sec: 7, usec: 11 },
            payload: Vec::new(),
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = WireChunk::read_from(&mut cursor).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn read_from_truncated_timestamp_is_io_error() {
        let mut cursor = std::io::Cursor::new([0u8; 7]);
        assert!(matches!(
            WireChunk::read_from(&mut cursor),
            Err(ProtoError::Io(_))
        ));
    }

    #[test]
    fn read_from_truncated_payload_body_is_io_error() {
        let mut buf = Vec::new();
        // full timestamp
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        // declared payload len = 4, provide only 2 bytes
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(&[0xAA, 0xBB]);
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            WireChunk::read_from(&mut cursor),
            Err(ProtoError::Io(_))
        ));
    }

    #[test]
    fn read_from_oversized_payload_len_is_payload_too_large() {
        let mut buf = Vec::new();
        // full timestamp
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&(DEFAULT_MAX_PAYLOAD_SIZE + 1).to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            WireChunk::read_from(&mut cursor),
            Err(ProtoError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn write_to_oversized_payload_is_payload_too_large() {
        let msg = WireChunk {
            timestamp: Timeval::default(),
            payload: vec![0u8; DEFAULT_MAX_PAYLOAD_SIZE as usize + 1],
        };
        let mut buf = Vec::new();
        assert!(matches!(
            msg.write_to(&mut buf),
            Err(ProtoError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn write_to_propagates_io_error() {
        let msg = WireChunk {
            timestamp: Timeval { sec: 1, usec: 2 },
            payload: vec![0xAA, 0xBB],
        };
        let mut writer = FailingWriter;
        assert!(matches!(msg.write_to(&mut writer), Err(ProtoError::Io(_))));
    }
}
