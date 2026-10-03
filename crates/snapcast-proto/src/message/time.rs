//! Time sync message (type=4).
//!
//! Sent by the client to the server and echoed back. Used to compute
//! the clock difference between client and server.
//!
//! Payload: a single [`Timeval`].
//!
//! The proto layer preserves this value as raw wire data; higher layers
//! interpret it for sync/latency calculations.

use std::io::{Read, Write};

use crate::message::base::ProtoError;
use crate::types::Timeval;

/// Time sync message payload (8 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Time {
    /// Raw time/sync value.
    pub latency: Timeval,
}

impl Time {
    /// Payload size in bytes.
    pub const SIZE: u32 = 8;

    /// Create a new Time message with zero latency.
    pub fn new() -> Self {
        Self {
            latency: Timeval::default(),
        }
    }

    /// Deserialize a Time message from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        Ok(Self {
            latency: Timeval::read_from(r)?,
        })
    }

    /// Serialize a Time message to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        self.latency.write_to(w)?;
        Ok(())
    }
}

impl Default for Time {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::base::ProtoError;

    struct FailingWriter;

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("forced write failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Payload bytes for latency = {sec: 0, usec: 1500}
    const TIME_PAYLOAD: [u8; 8] = [
        0x00, 0x00, 0x00, 0x00, // latency.sec = 0
        0xDC, 0x05, 0x00, 0x00, // latency.usec = 1500
    ];

    #[test]
    fn serialize() {
        let msg = Time {
            latency: Timeval { sec: 0, usec: 1500 },
        };
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), &TIME_PAYLOAD);
    }

    #[test]
    fn deserialize() {
        let mut cursor = std::io::Cursor::new(&TIME_PAYLOAD);
        let msg = Time::read_from(&mut cursor).unwrap();
        assert_eq!(msg.latency.sec, 0);
        assert_eq!(msg.latency.usec, 1500);
    }

    #[test]
    fn round_trip() {
        let original = Time {
            latency: Timeval {
                sec: 42,
                usec: 123_456,
            },
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), Time::SIZE as usize);
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Time::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn default_is_zero() {
        let msg = Time::new();
        assert_eq!(msg.latency, Timeval::default());
    }

    #[test]
    fn default_trait_matches_new() {
        assert_eq!(Time::default(), Time::new());
    }

    #[test]
    fn read_from_truncated_payload_is_io_error() {
        let truncated = [0u8; (Time::SIZE as usize) - 1];
        let mut cursor = std::io::Cursor::new(truncated);
        assert!(matches!(Time::read_from(&mut cursor), Err(ProtoError::Io(_))));
    }

    #[test]
    fn round_trip_negative_values() {
        let original = Time {
            latency: Timeval {
                sec: -3,
                usec: -250_000,
            },
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), Time::SIZE as usize);
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Time::read_from(&mut cursor).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn write_to_propagates_io_error() {
        let msg = Time {
            latency: Timeval {
                sec: 1,
                usec: 2,
            },
        };
        let mut writer = FailingWriter;
        assert!(matches!(msg.write_to(&mut writer), Err(ProtoError::Io(_))));
    }
}
