//! Protocol message types.

pub mod base;
pub mod client_info;
pub mod codec_header;
pub mod error;
pub mod factory;
pub mod hello;
pub mod server_settings;
pub mod time;
pub mod wire;
pub mod wire_chunk;

/// Message type identifiers matching the C++ `message_type` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// Base message (type 0), header only.
    Base,
    /// Codec header (type 1).
    CodecHeader,
    /// Encoded audio chunk (type 2).
    WireChunk,
    /// Server settings (type 3).
    ServerSettings,
    /// Time sync (type 4).
    Time,
    /// Client hello (type 5).
    Hello,
    // 6 = StreamTags (deprecated, but C++ server still sends it)
    /// Stream tags / metadata (type 6, deprecated).
    StreamTags,
    /// Client info (type 7).
    ClientInfo,
    /// Error (type 8).
    Error,
    /// Application-defined message (type 9+).
    #[cfg(feature = "custom-protocol")]
    Custom(u16),
    /// Unrecognized message type — payload is skipped/ignored.
    ///
    /// With `custom-protocol` enabled, [`MessageType::from_u16`] maps `9..`
    /// to [`MessageType::Custom`] instead of this variant.
    Unknown(u16),
}

impl MessageType {
    /// Parse from a raw `u16` value.
    ///
    /// Never fails. Known built-in values map to fixed variants.
    /// When `custom-protocol` is enabled, values `9..` map to `Custom(n)`;
    /// otherwise they map to `Unknown(n)`.
    pub fn from_u16(value: u16) -> Self {
        match value {
            0 => Self::Base,
            1 => Self::CodecHeader,
            2 => Self::WireChunk,
            3 => Self::ServerSettings,
            4 => Self::Time,
            5 => Self::Hello,
            6 => Self::StreamTags,
            7 => Self::ClientInfo,
            8 => Self::Error,
            #[cfg(feature = "custom-protocol")]
            9.. => Self::Custom(value),
            #[cfg(not(feature = "custom-protocol"))]
            _ => Self::Unknown(value),
        }
    }
}

impl From<MessageType> for u16 {
    fn from(mt: MessageType) -> Self {
        match mt {
            MessageType::Base => 0,
            MessageType::CodecHeader => 1,
            MessageType::WireChunk => 2,
            MessageType::ServerSettings => 3,
            MessageType::Time => 4,
            MessageType::Hello => 5,
            MessageType::StreamTags => 6,
            MessageType::ClientInfo => 7,
            MessageType::Error => 8,
            #[cfg(feature = "custom-protocol")]
            MessageType::Custom(id) => id,
            MessageType::Unknown(id) => id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all_message_types() {
        let types = [
            MessageType::Base,
            MessageType::CodecHeader,
            MessageType::WireChunk,
            MessageType::ServerSettings,
            MessageType::Time,
            MessageType::Hello,
            MessageType::StreamTags,
            MessageType::ClientInfo,
            MessageType::Error,
        ];
        for mt in types {
            let raw: u16 = mt.into();
            assert_eq!(MessageType::from_u16(raw), mt);
        }
    }

    #[test]
    fn unknown_message_type_returns_unknown() {
        assert_eq!(MessageType::from_u16(6), MessageType::StreamTags);
        #[cfg(not(feature = "custom-protocol"))]
        {
            assert_eq!(MessageType::from_u16(9), MessageType::Unknown(9));
            assert_eq!(
                MessageType::from_u16(u16::MAX),
                MessageType::Unknown(u16::MAX)
            );
        }
        #[cfg(feature = "custom-protocol")]
        {
            assert_eq!(MessageType::from_u16(9), MessageType::Custom(9));
            assert_eq!(
                MessageType::from_u16(u16::MAX),
                MessageType::Custom(u16::MAX)
            );
        }
    }

    #[test]
    fn unknown_variant_round_trips_raw_id() {
        let raw: u16 = MessageType::Unknown(0xBEEF).into();
        assert_eq!(raw, 0xBEEF);
    }

    #[test]
    fn from_u16_and_back_preserves_wire_id() {
        let candidates = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, u16::MAX];
        for raw in candidates {
            let round_trip: u16 = MessageType::from_u16(raw).into();
            assert_eq!(round_trip, raw);
        }
    }

    #[cfg(feature = "custom-protocol")]
    #[test]
    fn custom_variant_round_trips_raw_id() {
        let raw: u16 = MessageType::Custom(9).into();
        assert_eq!(raw, 9);
        let raw_max: u16 = MessageType::Custom(u16::MAX).into();
        assert_eq!(raw_max, u16::MAX);
    }
}

/// Custom message for application-defined protocol extensions (type 9+).
#[cfg(feature = "custom-protocol")]
#[derive(Debug, Clone)]
pub struct CustomMessage {
    /// Message type ID (typically 9+; values 0..=8 are reserved by core protocol).
    pub type_id: u16,
    /// Raw payload bytes.
    pub payload: Vec<u8>,
}

#[cfg(feature = "custom-protocol")]
impl CustomMessage {
    /// Create a new custom message.
    ///
    /// Panics when `type_id < 9`, because those IDs are core
    /// protocol message types.
    pub fn new(type_id: u16, payload: impl Into<Vec<u8>>) -> Self {
        assert!(type_id >= 9, "custom message type_id must be >= 9");
        Self {
            type_id,
            payload: payload.into(),
        }
    }
}

#[cfg(all(test, feature = "custom-protocol"))]
mod custom_tests {
    use super::*;

    #[test]
    fn custom_message_new_sets_fields() {
        let msg = CustomMessage::new(42, b"hello".to_vec());
        assert_eq!(msg.type_id, 42);
        assert_eq!(msg.payload, b"hello");
    }

    #[test]
    #[should_panic(expected = "custom message type_id must be >= 9")]
    fn custom_message_new_panics_for_reserved_id() {
        let _ = CustomMessage::new(8, []);
    }
}
