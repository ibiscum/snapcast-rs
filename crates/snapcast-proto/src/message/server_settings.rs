//! Server Settings message (type=3).
//!
//! Sent from server to client. Contains buffer size, latency, volume, and mute state.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Server settings JSON payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// Buffer size in milliseconds.
    pub buffer_ms: i32,
    /// Additional latency in milliseconds.
    pub latency: i32,
    /// Playback volume (typically 0–100).
    ///
    /// This proto layer preserves wire values as-is and does not enforce
    /// policy bounds; range clamping/validation belongs to higher layers.
    pub volume: u16,
    /// Whether the client is muted.
    pub muted: bool,
}

impl ServerSettings {
    /// Wire size of the JSON payload including length prefix.
    pub fn wire_size(&self) -> u32 {
        wire::json_wire_size(self)
    }

    /// Deserialize server settings from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        wire::read_json(r)
    }

    /// Serialize server settings to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        wire::write_json(w, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_MAX_PAYLOAD_SIZE;
    use crate::message::base::ProtoError;

    #[test]
    fn round_trip() {
        let original = ServerSettings {
            buffer_ms: 1000,
            latency: 0,
            volume: 100,
            muted: false,
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ServerSettings::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn deserialize_cpp_json() {
        let json = r#"{"bufferMs":1000,"latency":0,"muted":false,"volume":100}"#;
        let ss: ServerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(ss.buffer_ms, 1000);
        assert_eq!(ss.volume, 100);
        assert!(!ss.muted);
    }

    #[test]
    fn json_field_names_match_cpp() {
        let ss = ServerSettings {
            buffer_ms: 500,
            latency: 10,
            volume: 80,
            muted: true,
        };
        let json_str = serde_json::to_string(&ss).unwrap();
        assert!(json_str.contains("\"bufferMs\""));
        assert!(json_str.contains("\"latency\""));
        assert!(json_str.contains("\"volume\""));
        assert!(json_str.contains("\"muted\""));
    }

    #[test]
    fn wire_size_matches_serialized_length() {
        let ss = ServerSettings {
            buffer_ms: 250,
            latency: 15,
            volume: 42,
            muted: true,
        };
        let mut buf = Vec::new();
        ss.write_to(&mut buf).unwrap();
        assert_eq!(ss.wire_size(), buf.len() as u32);
    }

    #[test]
    fn read_from_truncated_json_payload_errors() {
        // Declared length is 12 bytes, payload provides only 7 bytes.
        let mut buf = Vec::new();
        buf.extend_from_slice(&12u32.to_le_bytes());
        buf.extend_from_slice(b"{\"buffe");
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            ServerSettings::read_from(&mut cursor),
            Err(ProtoError::Io(_))
        ));
    }

    #[test]
    fn read_from_invalid_json_errors() {
        let bad = b"{\"bufferMs\":,}";
        let mut buf = Vec::new();
        buf.extend_from_slice(&(bad.len() as u32).to_le_bytes());
        buf.extend_from_slice(bad);
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            ServerSettings::read_from(&mut cursor),
            Err(ProtoError::Json(_))
        ));
    }

    #[test]
    fn oversized_wire_payload_is_payload_too_large() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(DEFAULT_MAX_PAYLOAD_SIZE + 1).to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            ServerSettings::read_from(&mut cursor),
            Err(ProtoError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn invalid_utf8_wire_payload_is_utf8_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&[0xFF, 0xFF]);
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            ServerSettings::read_from(&mut cursor),
            Err(ProtoError::Utf8(_))
        ));
    }

    #[test]
    fn out_of_range_volume_is_preserved_at_proto_layer() {
        let ss = ServerSettings {
            buffer_ms: 1000,
            latency: 0,
            volume: u16::MAX,
            muted: true,
        };
        let mut buf = Vec::new();
        ss.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ServerSettings::read_from(&mut cursor).unwrap();
        assert_eq!(decoded.volume, u16::MAX);
        assert!(decoded.muted);
    }

    #[test]
    fn serde_missing_required_field_fails() {
        let json = r#"{"bufferMs":1000,"latency":0,"muted":false}"#;
        assert!(serde_json::from_str::<ServerSettings>(json).is_err());
    }

    #[test]
    fn serde_unknown_field_is_ignored() {
        let json = r#"{"bufferMs":1000,"latency":0,"volume":100,"muted":false,"extra":"ignored"}"#;
        let ss: ServerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(ss.buffer_ms, 1000);
        assert_eq!(ss.volume, 100);
        assert!(!ss.muted);
    }
}
