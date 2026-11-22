//! Null decoder — accepts the stream and passes payload bytes through unchanged.
//!
//! This mirrors upstream `snapclient` behavior for the `null` codec.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_proto::message::codec_header::CodecHeader;

use crate::stream::SampleEncoding;

use super::Decoder;

/// Decoder for the Snapcast `null` wire codec.
#[derive(Default)]
pub struct NullDecoder;

impl NullDecoder {
    /// Construct a null decoder.
    pub fn new() -> Self {
        Self
    }
}

impl Decoder for NullDecoder {
    fn set_header(&mut self, _header: &CodecHeader) -> Result<SampleFormat> {
        Ok(snapcast_proto::DEFAULT_SAMPLE_FORMAT)
    }

    fn decode(&mut self, _data: &mut Vec<u8>) -> Result<bool> {
        Ok(true)
    }

    fn output_encoding(&self) -> SampleEncoding {
        SampleEncoding::PcmInt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_codec_ignores_header_and_returns_default_format() {
        let mut dec = NullDecoder::new();
        let header = CodecHeader {
            codec: "null".into(),
            payload: vec![1, 2, 3],
        };
        let format = dec.set_header(&header).unwrap();
        assert_eq!(format, snapcast_proto::DEFAULT_SAMPLE_FORMAT);
    }

    #[test]
    fn null_codec_is_passthrough() {
        let mut dec = NullDecoder::new();
        let mut bytes = vec![0xAA, 0xBB, 0xCC];
        assert!(dec.decode(&mut bytes).unwrap());
        assert_eq!(bytes, vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(dec.output_encoding(), SampleEncoding::PcmInt);
    }
}
