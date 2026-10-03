//! Client Info message (type=7).
//!
//! Sent from client to server to report volume and mute state changes.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Client info JSON payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Playback volume (typically 0–100).
    ///
    /// This proto layer preserves wire values as-is and does not enforce
    /// policy bounds; range clamping/validation belongs to higher layers.
    pub volume: u16,
    /// Whether the client is muted.
    pub muted: bool,
}

impl ClientInfo {
    /// Wire size of the JSON payload including length prefix.
    pub fn wire_size(&self) -> u32 {
        wire::json_wire_size(self)
    }

    /// Deserialize client info from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        wire::read_json(r)
    }

    /// Serialize client info to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        wire::write_json(w, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::base::ProtoError;

    #[test]
    fn round_trip() {
        let original = ClientInfo {
            volume: 75,
            muted: false,
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ClientInfo::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn deserialize_cpp_json() {
        let json = r#"{"volume":100,"muted":false}"#;
        let ci: ClientInfo = serde_json::from_str(json).unwrap();
        assert_eq!(ci.volume, 100);
        assert!(!ci.muted);
    }

    #[test]
    fn wire_size_matches_serialized_length() {
        let ci = ClientInfo {
            volume: 42,
            muted: true,
        };
        let mut buf = Vec::new();
        ci.write_to(&mut buf).unwrap();
        assert_eq!(ci.wire_size(), buf.len() as u32);
    }

    #[test]
    fn read_from_truncated_json_payload_errors() {
        // Declared length is 10 bytes, payload provides only 5 bytes.
        let mut buf = Vec::new();
        buf.extend_from_slice(&10u32.to_le_bytes());
        buf.extend_from_slice(b"{\"vol");
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(ClientInfo::read_from(&mut cursor), Err(ProtoError::Io(_))));
    }

    #[test]
    fn read_from_invalid_json_errors() {
        let bad = b"{\"volume\":,}";
        let mut buf = Vec::new();
        buf.extend_from_slice(&(bad.len() as u32).to_le_bytes());
        buf.extend_from_slice(bad);
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            ClientInfo::read_from(&mut cursor),
            Err(ProtoError::Json(_))
        ));
    }

    #[test]
    fn out_of_range_volume_is_preserved_at_proto_layer() {
        let ci = ClientInfo {
            volume: u16::MAX,
            muted: true,
        };
        let mut buf = Vec::new();
        ci.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ClientInfo::read_from(&mut cursor).unwrap();
        assert_eq!(decoded.volume, u16::MAX);
        assert!(decoded.muted);
    }

    #[test]
    fn serde_missing_required_field_fails() {
        let json = r#"{"volume":50}"#;
        assert!(serde_json::from_str::<ClientInfo>(json).is_err());
    }

    #[test]
    fn serde_unknown_field_is_ignored() {
        let json = r#"{"volume":50,"muted":false,"extra":"ignored"}"#;
        let ci: ClientInfo = serde_json::from_str(json).unwrap();
        assert_eq!(ci.volume, 50);
        assert!(!ci.muted);
    }
}
