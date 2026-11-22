//! F32 LZ4 decoder — lossless compressed f32 audio.
//!
//! Wire format:
//! - Header: "F32L" + sample_rate(u32) + channels(u16) + bits(u16), optionally
//!   followed by an encryption marker and salt
//! - Chunks: size-prefixed LZ4 blocks, optionally encrypted
//!
//! Output: complete interleaved little-endian f32 frames, at most 16 MiB per chunk.

use anyhow::{Result, bail, ensure};
use snapcast_proto::SampleFormat;
use snapcast_proto::f32lz4::{
    F32LZ4_ENC_HEADER_LEN, F32LZ4_ENC_MARKER, F32LZ4_HEADER_LEN, F32LZ4_MAGIC,
};
use snapcast_proto::message::codec_header::CodecHeader;

use crate::decoder::Decoder;
use crate::stream::SampleEncoding;

const MAX_DECODED_CHUNK_BYTES: usize = 16 * 1024 * 1024;

fn valid_decoded_size(bytes: usize, frame_size: usize) -> bool {
    (1..=MAX_DECODED_CHUNK_BYTES).contains(&bytes)
        && frame_size != 0
        && bytes.is_multiple_of(frame_size)
}

/// F32 LZ4 decoder.
pub struct F32Lz4Decoder {
    sample_format: SampleFormat,
    #[cfg(feature = "encryption")]
    decryptor: Option<crate::crypto::ChunkDecryptor>,
    #[cfg(feature = "encryption")]
    encryption_psk: Option<String>,
}

impl Decoder for F32Lz4Decoder {
    fn set_header(&mut self, header: &CodecHeader) -> Result<SampleFormat> {
        if header.payload.len() < F32LZ4_HEADER_LEN {
            bail!("F32LZ4 header too small");
        }
        if &header.payload[..F32LZ4_MAGIC.len()] != F32LZ4_MAGIC {
            bail!("not an F32LZ4 header");
        }
        let rate = u32::from_le_bytes(header.payload[4..8].try_into().unwrap());
        let channels = u16::from_le_bytes(header.payload[8..10].try_into().unwrap());
        let bits = u16::from_le_bytes(header.payload[10..12].try_into().unwrap());
        ensure!(rate != 0, "F32LZ4 sample rate must be nonzero");
        ensure!(bits == 32, "F32LZ4 requires 32-bit samples");
        ensure!(
            channels != 0 && channels.checked_mul(4).is_some(),
            "invalid F32LZ4 channel count: {channels}"
        );

        let extension = &header.payload[F32LZ4_HEADER_LEN..];
        if !extension.is_empty() {
            ensure!(
                header.payload.len() == F32LZ4_ENC_HEADER_LEN
                    && extension.starts_with(F32LZ4_ENC_MARKER),
                "invalid F32LZ4 encryption extension"
            );
            #[cfg(not(feature = "encryption"))]
            bail!("server requires F32LZ4 encryption but this client was built without it");
        }

        #[cfg(feature = "encryption")]
        {
            self.decryptor = if extension.is_empty() {
                None
            } else {
                let psk = self.encryption_psk.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("Server requires encryption but no encryption_psk configured")
                })?;
                let salt = &extension[F32LZ4_ENC_MARKER.len()..];
                tracing::info!("F32LZ4 decryption enabled");
                Some(crate::crypto::ChunkDecryptor::new(psk, salt))
            };
        }

        self.sample_format = SampleFormat::new(rate, bits, channels);
        tracing::info!(rate, channels, bits, "F32LZ4 decoder initialized");
        Ok(self.sample_format)
    }

    fn decode(&mut self, data: &mut Vec<u8>) -> Result<bool> {
        if data.is_empty() {
            return Ok(false);
        }

        #[cfg(feature = "encryption")]
        let decrypted;
        #[cfg(feature = "encryption")]
        let compressed = if let Some(ref dec) = self.decryptor {
            match dec.decrypt(data) {
                Ok(bytes) => {
                    decrypted = bytes;
                    decrypted.as_slice()
                }
                Err(e) => {
                    tracing::warn!(error = %e, "F32LZ4 decryption failed");
                    return Ok(false);
                }
            }
        } else {
            data.as_slice()
        };
        #[cfg(not(feature = "encryption"))]
        let compressed = data.as_slice();

        let Some((size, block)) = compressed.split_first_chunk::<4>() else {
            tracing::warn!("F32LZ4 chunk is missing its decoded size");
            return Ok(false);
        };
        let declared = u32::from_le_bytes(*size) as usize;
        let frame_size = usize::from(self.sample_format.channels()) * 4;
        if !valid_decoded_size(declared, frame_size) {
            tracing::warn!(declared, frame_size, "invalid F32LZ4 decoded chunk size");
            return Ok(false);
        }
        let mut decompressed = vec![0; declared];
        match lz4_flex::block::decompress_into(block, &mut decompressed) {
            Ok(actual) if actual == declared => {
                tracing::trace!(
                    compressed = data.len(),
                    decompressed = decompressed.len(),
                    "F32LZ4 decoded"
                );
                *data = decompressed;
                Ok(true)
            }
            Ok(actual) => {
                tracing::warn!(actual, declared, "F32LZ4 decoded size mismatch");
                Ok(false)
            }
            Err(e) => {
                tracing::warn!(error = %e, "F32LZ4 decompress failed");
                Ok(false)
            }
        }
    }

    fn output_encoding(&self) -> SampleEncoding {
        SampleEncoding::Float32
    }
}

/// Create an F32Lz4Decoder.
#[cfg(feature = "encryption")]
pub fn create(encryption_psk: Option<&str>) -> F32Lz4Decoder {
    F32Lz4Decoder {
        sample_format: SampleFormat::default(),
        decryptor: None,
        encryption_psk: encryption_psk.map(String::from),
    }
}

/// Create an F32Lz4Decoder.
#[cfg(not(feature = "encryption"))]
pub fn create() -> F32Lz4Decoder {
    F32Lz4Decoder {
        sample_format: SampleFormat::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_header(payload: Vec<u8>) -> CodecHeader {
        CodecHeader {
            codec: "f32lz4".into(),
            payload,
        }
    }

    fn valid_header_payload(rate: u32, channels: u16, bits: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(F32LZ4_MAGIC);
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&channels.to_le_bytes());
        h.extend_from_slice(&bits.to_le_bytes());
        h
    }

    fn plain_decoder() -> F32Lz4Decoder {
        #[cfg(feature = "encryption")]
        let decoder = create(None);
        #[cfg(not(feature = "encryption"))]
        let decoder = create();
        decoder
    }

    fn encrypted_header_payload(rate: u32) -> Vec<u8> {
        let mut header = valid_header_payload(rate, 2, 32);
        header.extend_from_slice(F32LZ4_ENC_MARKER);
        header.resize(F32LZ4_ENC_HEADER_LEN, 7);
        header
    }

    #[test]
    fn roundtrip() {
        // Encode
        let f32_samples: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect();
        let f32_bytes: Vec<u8> = f32_samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let compressed = lz4_flex::compress_prepend_size(&f32_bytes);

        // Decode
        #[cfg(feature = "encryption")]
        let mut dec = create(None);
        #[cfg(not(feature = "encryption"))]
        let mut dec = create();
        let header = mk_header(valid_header_payload(48000, 2, 32));
        let sf = dec.set_header(&header).unwrap();
        assert_eq!(sf, SampleFormat::new(48000, 32, 2));

        let mut data = compressed;
        assert!(dec.decode(&mut data).unwrap());
        assert_eq!(data, f32_bytes);
    }

    #[test]
    fn set_header_rejects_too_small_payload() {
        #[cfg(feature = "encryption")]
        let mut dec = create(None);
        #[cfg(not(feature = "encryption"))]
        let mut dec = create();
        let header = mk_header(vec![0u8; F32LZ4_HEADER_LEN - 1]);
        assert!(dec.set_header(&header).is_err());
    }

    #[test]
    fn set_header_rejects_wrong_magic() {
        #[cfg(feature = "encryption")]
        let mut dec = create(None);
        #[cfg(not(feature = "encryption"))]
        let mut dec = create();
        let mut bad = valid_header_payload(48000, 2, 32);
        bad[0..4].copy_from_slice(b"NOPE");
        let header = mk_header(bad);
        assert!(dec.set_header(&header).is_err());
    }

    #[test]
    fn decode_empty_input_returns_false() {
        #[cfg(feature = "encryption")]
        let mut dec = create(None);
        #[cfg(not(feature = "encryption"))]
        let mut dec = create();
        let mut data = Vec::new();
        assert!(!dec.decode(&mut data).unwrap());
    }

    #[test]
    fn decode_invalid_lz4_returns_false() {
        #[cfg(feature = "encryption")]
        let mut dec = create(None);
        #[cfg(not(feature = "encryption"))]
        let mut dec = create();
        dec.set_header(&mk_header(valid_header_payload(48000, 2, 32)))
            .unwrap();
        let mut data = lz4_flex::compress_prepend_size(&[1u8; 8]);
        data.truncate(data.len() - 1);
        assert!(!dec.decode(&mut data).unwrap());
    }

    #[test]
    fn invalid_sample_formats_do_not_replace_active_format() {
        let mut dec = plain_decoder();
        let original = SampleFormat::new(48000, 32, 2);
        dec.set_header(&mk_header(valid_header_payload(48000, 2, 32)))
            .unwrap();
        for (rate, channels, bits) in [
            (0, 2, 32),
            (44100, 0, 32),
            (44100, 2, 16),
            (44100, u16::MAX, 32),
        ] {
            assert!(
                dec.set_header(&mk_header(valid_header_payload(rate, channels, bits)))
                    .is_err()
            );
            assert_eq!(dec.sample_format, original);
        }
    }

    #[test]
    fn incomplete_encryption_extensions_are_rejected() {
        let mut dec = plain_decoder();
        let header = encrypted_header_payload(48000);
        for end in F32LZ4_HEADER_LEN + 1..F32LZ4_ENC_HEADER_LEN {
            let err = dec
                .set_header(&mk_header(header[..end].to_vec()))
                .unwrap_err();
            assert!(
                err.to_string()
                    .contains("invalid F32LZ4 encryption extension")
            );
        }
    }

    #[test]
    fn encrypted_header_requires_encryption_support_and_a_key() {
        let mut dec = plain_decoder();
        dec.set_header(&mk_header(valid_header_payload(48000, 2, 32)))
            .unwrap();
        let err = dec
            .set_header(&mk_header(encrypted_header_payload(44100)))
            .unwrap_err();
        #[cfg(feature = "encryption")]
        assert!(err.to_string().contains("no encryption_psk configured"));
        #[cfg(not(feature = "encryption"))]
        assert!(err.to_string().contains("built without it"));
        assert_eq!(dec.sample_format, SampleFormat::new(48000, 32, 2));
    }

    #[cfg(feature = "encryption")]
    #[test]
    fn plain_header_replaces_encrypted_mode_after_failed_reinitialization() {
        let mut dec = create(Some("fixture-key"));
        dec.set_header(&mk_header(encrypted_header_payload(48000)))
            .unwrap();
        let mut truncated = encrypted_header_payload(44100);
        truncated.pop();
        assert!(dec.set_header(&mk_header(truncated)).is_err());
        assert_eq!(dec.sample_format, SampleFormat::new(48000, 32, 2));
        let samples = [0.25f32.to_le_bytes(), (-0.5f32).to_le_bytes()].concat();
        let compressed = lz4_flex::compress_prepend_size(&samples);
        assert!(!dec.decode(&mut compressed.clone()).unwrap());

        dec.set_header(&mk_header(valid_header_payload(44100, 2, 32)))
            .unwrap();
        let mut data = compressed;
        assert!(dec.decode(&mut data).unwrap());
        assert_eq!(data, samples);
    }

    #[test]
    fn decoded_chunks_must_contain_complete_frames_and_match_declared_size() {
        let mut dec = plain_decoder();
        dec.set_header(&mk_header(valid_header_payload(48000, 2, 32)))
            .unwrap();
        let partial = lz4_flex::compress_prepend_size(&[0; 4]);
        let mut wrong_size = lz4_flex::compress_prepend_size(&[0; 8]);
        wrong_size[..4].copy_from_slice(&16u32.to_le_bytes());
        for original in [partial, wrong_size] {
            let mut data = original.clone();
            assert!(!dec.decode(&mut data).unwrap());
            assert_eq!(data, original);
        }
    }

    #[test]
    fn decoded_size_limit_is_inclusive_for_complete_frames() {
        assert!(valid_decoded_size(MAX_DECODED_CHUNK_BYTES, 8));
        assert!(!valid_decoded_size(MAX_DECODED_CHUNK_BYTES + 8, 8));
    }

    #[test]
    fn output_encoding_is_float32() {
        #[cfg(feature = "encryption")]
        let dec = create(None);
        #[cfg(not(feature = "encryption"))]
        let dec = create();
        assert_eq!(dec.output_encoding(), SampleEncoding::Float32);
    }
}
