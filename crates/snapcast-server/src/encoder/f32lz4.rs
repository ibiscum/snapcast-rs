//! F32 LZ4 encoder — lossless compressed f32 audio.
//!
//! Skips PCM conversion entirely. The wire format is:
//! - Header: "F32L" magic + sample_rate(u32) + channels(u16) + bits(u16) = 12 bytes
//! - Chunks: LZ4-compressed f32 samples (lz4_flex prepend_size format)

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;
#[cfg(feature = "encryption")]
use snapcast_proto::f32lz4::{F32LZ4_ENC_MARKER, F32LZ4_SALT_LEN};
#[cfg(all(feature = "encryption", test))]
use snapcast_proto::f32lz4::F32LZ4_ENC_HEADER_LEN;
use snapcast_proto::f32lz4::{F32LZ4_HEADER_LEN, F32LZ4_MAGIC};

use super::{EncodedChunk, Encoder};
use crate::AudioData;

/// Fill buffer with random bytes (uses system RNG).
#[cfg(feature = "encryption")]
fn random_bytes(buf: &mut [u8]) {
    getrandom::fill(buf).expect("OS RNG unavailable");
}

/// F32 LZ4 encoder — compresses f32 audio with LZ4.
pub struct F32Lz4Encoder {
    format: SampleFormat,
    header: Vec<u8>,
    warned: bool,
    #[cfg(feature = "encryption")]
    encryptor: Option<crate::crypto::ChunkEncryptor>,
}

impl F32Lz4Encoder {
    /// Create a new F32 LZ4 encoder.
    pub fn new(format: SampleFormat) -> Self {
        tracing::info!(
            rate = format.rate(),
            channels = format.channels(),
            "F32LZ4 encoder initialized"
        );
        let mut header = Vec::with_capacity(F32LZ4_HEADER_LEN);
        header.extend_from_slice(F32LZ4_MAGIC);
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&format.channels().to_le_bytes());
        header.extend_from_slice(&32u16.to_le_bytes()); // bits = 32 (f32)
        Self {
            format,
            header,
            warned: false,
            #[cfg(feature = "encryption")]
            encryptor: None,
        }
    }

    /// Enable encryption with a pre-shared key. Appends salt to the codec header.
    #[cfg(feature = "encryption")]
    pub fn with_encryption(mut self, psk: &str) -> Self {
        let mut salt = [0u8; F32LZ4_SALT_LEN];
        random_bytes(&mut salt);
        self.encryptor = Some(crate::crypto::ChunkEncryptor::new(psk, &salt));
        // Normalize header to a single encryption marker + salt segment.
        self.header.truncate(F32LZ4_HEADER_LEN);
        self.header.extend_from_slice(F32LZ4_ENC_MARKER);
        self.header.extend_from_slice(&salt);
        tracing::info!("F32LZ4 encryption enabled");
        self
    }
}

impl Encoder for F32Lz4Encoder {
    fn name(&self) -> &str {
        snapcast_proto::CODEC_F32LZ4
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let channels = self.format.channels() as usize;
        if channels == 0 {
            bail!("f32lz4 encoder requires non-zero channel count");
        }

        // f32lz4 compresses f32 bytes directly
        let f32_bytes: Vec<u8> = match input {
            AudioData::F32(samples) => {
                // Zero conversion — reinterpret f32 as bytes
                samples.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
            AudioData::Pcm(pcm) => {
                // Convert integer PCM → f32 → bytes
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "f32lz4",
                        bits = self.format.bits(),
                        "PCM input requires conversion to f32 — consider sending F32 directly"
                    );
                }
                let f32_samples = super::pcm_to_f32(pcm, self.format.bits())?;
                f32_samples.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
        };

        let frames = f32_bytes.len() / (4 * channels);
        tracing::trace!(input_bytes = f32_bytes.len(), frames, "F32LZ4 encoding");
        let compressed = lz4_flex::compress_prepend_size(&f32_bytes);

        #[cfg(feature = "encryption")]
        let data = if let Some(ref mut enc) = self.encryptor {
            enc.encrypt(&compressed)
                .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?
        } else {
            compressed
        };
        #[cfg(not(feature = "encryption"))]
        let data = compressed;

        Ok(EncodedChunk { data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_format() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let enc = F32Lz4Encoder::new(fmt);
        assert_eq!(&enc.header()[..4], F32LZ4_MAGIC.as_slice());
        assert_eq!(enc.header().len(), F32LZ4_HEADER_LEN);
    }

    #[test]
    fn encode_compresses_f32() {
        let fmt = SampleFormat::new(48000, 32, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let samples = vec![0.0f32; 960 * 2]; // 960 frames, stereo
        let result = enc.encode(&AudioData::F32(samples)).unwrap();
        assert!(!result.data.is_empty());
    }

    #[test]
    fn encode_f32_round_trips_through_lz4() {
        let fmt = SampleFormat::new(48_000, 32, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let samples = vec![0.0f32, -0.5, 0.25, 1.0, -1.0, 0.75];
        let encoded = enc.encode(&AudioData::F32(samples.clone())).unwrap();
        let decoded = lz4_flex::decompress_size_prepended(&encoded.data).unwrap();
        let expected: Vec<u8> = samples.into_iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn encode_compresses_pcm() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let pcm = vec![0u8; 960 * 4]; // 960 frames, 16-bit stereo
        let result = enc.encode(&AudioData::Pcm(pcm)).unwrap();
        assert!(!result.data.is_empty());
    }

    #[test]
    fn encode_pcm_round_trips_through_lz4() {
        let fmt = SampleFormat::new(48_000, 16, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let pcm = vec![0x01, 0x00, 0xFF, 0x7F, 0x00, 0x80, 0x00, 0x00];
        let encoded = enc.encode(&AudioData::Pcm(pcm.clone())).unwrap();
        let decoded = lz4_flex::decompress_size_prepended(&encoded.data).unwrap();
        let f32_samples = crate::encoder::pcm_to_f32(&pcm, 16).unwrap();
        let expected: Vec<u8> = f32_samples
            .into_iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn encode_with_zero_channels_is_error() {
        let fmt = SampleFormat::new(48_000, 32, 0);
        let mut enc = F32Lz4Encoder::new(fmt);
        let err = match enc.encode(&AudioData::F32(vec![0.0, 1.0])) {
            Ok(_) => panic!("expected channel-count error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("non-zero channel count"));
    }

    #[cfg(feature = "encryption")]
    #[test]
    fn with_encryption_is_idempotent_and_sets_header_layout() {
        let fmt = SampleFormat::new(48_000, 32, 2);
        let enc = F32Lz4Encoder::new(fmt)
            .with_encryption("k1")
            .with_encryption("k2");
        let header = enc.header();
        assert_eq!(header.len(), F32LZ4_ENC_HEADER_LEN);
        assert_eq!(&header[..F32LZ4_HEADER_LEN], {
            let mut base = Vec::new();
            base.extend_from_slice(F32LZ4_MAGIC);
            base.extend_from_slice(&48_000u32.to_le_bytes());
            base.extend_from_slice(&2u16.to_le_bytes());
            base.extend_from_slice(&32u16.to_le_bytes());
            base
        }.as_slice());
        assert_eq!(
            &header[F32LZ4_HEADER_LEN..F32LZ4_HEADER_LEN + F32LZ4_ENC_MARKER.len()],
            F32LZ4_ENC_MARKER
        );
    }

    #[cfg(feature = "encryption")]
    #[test]
    fn encode_with_encryption_can_be_decrypted() {
        let fmt = SampleFormat::new(48_000, 32, 2);
        let mut enc = F32Lz4Encoder::new(fmt).with_encryption("secret");
        let header = enc.header().to_vec();
        let salt = &header[F32LZ4_HEADER_LEN + F32LZ4_ENC_MARKER.len()..F32LZ4_ENC_HEADER_LEN];

        let samples = vec![0.0f32, 0.5, -0.25, 1.0];
        let out = enc.encode(&AudioData::F32(samples.clone())).unwrap();
        let dec = crate::crypto::ChunkDecryptor::new("secret", salt);
        let compressed = dec.decrypt(&out.data).unwrap();
        let decoded = lz4_flex::decompress_size_prepended(&compressed).unwrap();
        let expected: Vec<u8> = samples.into_iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(decoded, expected);
    }
}
