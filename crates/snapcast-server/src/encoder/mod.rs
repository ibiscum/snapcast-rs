//! Audio encoders — PCM, FLAC, Opus, Vorbis, F32LZ4.

#[cfg(feature = "f32lz4")]
pub mod f32lz4;
#[cfg(feature = "flac")]
pub mod flac;
#[cfg(feature = "opus")]
pub mod opus;
pub mod pcm;
#[cfg(feature = "vorbis")]
pub mod vorbis;

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;

use crate::AudioData;

/// Result of encoding an audio chunk.
pub(crate) struct EncodedChunk {
    /// Encoded audio data.
    pub data: Vec<u8>,
}

/// Trait for audio encoders.
///
/// Each encoder accepts [`AudioData`] (F32 or Pcm) and handles conversion
/// internally. This keeps format-specific logic in the encoder, not the caller.
pub(crate) trait Encoder: Send {
    /// Codec name (e.g. "flac", "pcm", "opus", "ogg", "f32lz4").
    fn name(&self) -> &str;

    /// Codec header bytes sent to clients before audio data.
    fn header(&self) -> &[u8];

    /// Encode an audio chunk. Accepts F32 or Pcm input.
    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk>;
}

/// Configuration for creating an encoder.
#[derive(Debug, Clone)]
pub(crate) struct EncoderConfig {
    /// Codec name: "pcm", "flac", "opus", "ogg", "f32lz4".
    pub codec: String,
    /// Audio sample format.
    pub format: SampleFormat,
    /// Codec-specific options (e.g. FLAC compression level).
    pub options: String,
    /// Pre-shared key for f32lz4 encryption. `None` = no encryption.
    #[cfg(feature = "encryption")]
    pub encryption_psk: Option<String>,
}

/// Create an encoder from config.
pub(crate) fn create(config: &EncoderConfig) -> Result<Box<dyn Encoder>> {
    #[allow(unused_variables)]
    let EncoderConfig {
        codec,
        format,
        options,
        ..
    } = config;
    let format = *format;
    match codec.as_str() {
        snapcast_proto::CODEC_PCM => Ok(Box::new(pcm::PcmEncoder::new(format))),
        #[cfg(feature = "flac")]
        snapcast_proto::CODEC_FLAC => Ok(Box::new(flac::FlacEncoder::new(format, options)?)),
        #[cfg(feature = "opus")]
        snapcast_proto::CODEC_OPUS => Ok(Box::new(opus::OpusEncoder::new(format, options)?)),
        #[cfg(feature = "vorbis")]
        snapcast_proto::CODEC_OGG => Ok(Box::new(vorbis::VorbisEncoder::new(format, options)?)),
        #[cfg(feature = "f32lz4")]
        snapcast_proto::CODEC_F32LZ4 => {
            let enc = f32lz4::F32Lz4Encoder::new(format);
            #[cfg(feature = "encryption")]
            let enc = if let Some(ref key) = config.encryption_psk {
                enc.with_encryption(key)
            } else {
                enc
            };
            Ok(Box::new(enc))
        }
        other => anyhow::bail!("unsupported codec: {other} (check enabled features)"),
    }
}

/// Convert f32 samples to PCM bytes at the given bit depth.
/// Shared helper for encoders that need integer PCM input.
/// Returns an error for unsupported bit depths.
pub(crate) fn f32_to_pcm(samples: &[f32], bits: u16) -> Result<Vec<u8>> {
    let pcm = match bits {
        16 => {
            let mut buf = Vec::with_capacity(samples.len() * 2);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        24 => {
            let mut buf = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * snapcast_proto::PCM_24BIT_MAX) as i32;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        32 => {
            let mut buf = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * i32::MAX as f32) as i32;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        _ => bail!("unsupported PCM bit depth for f32_to_pcm: {bits}"),
    };
    Ok(pcm)
}

/// Convert PCM bytes to f32 samples at the given bit depth.
/// Shared helper for encoders that need f32 input.
/// Returns an error for unsupported bit depths.
#[cfg(any(feature = "f32lz4", feature = "opus", test))]
pub(crate) fn pcm_to_f32(pcm: &[u8], bits: u16) -> Result<Vec<f32>> {
    let samples = match bits {
        16 => pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32)
            .collect(),
        24 => pcm
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / snapcast_proto::PCM_24BIT_MAX
            })
            .collect(),
        32 => pcm
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / i32::MAX as f32)
            .collect(),
        _ => bail!("unsupported PCM bit depth for pcm_to_f32: {bits}"),
    };
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_pcm_encoder_from_config() {
        let cfg = EncoderConfig {
            codec: snapcast_proto::CODEC_PCM.to_string(),
            format: SampleFormat::new(48_000, 16, 2),
            options: String::new(),
            #[cfg(feature = "encryption")]
            encryption_psk: None,
        };
        let enc = create(&cfg).expect("pcm encoder should be created");
        assert_eq!(enc.name(), snapcast_proto::CODEC_PCM);
        assert_eq!(enc.header().len(), 44);
        assert_eq!(&enc.header()[..4], b"RIFF");
    }

    #[test]
    fn create_rejects_unknown_codec() {
        let cfg = EncoderConfig {
            codec: "definitely-not-a-codec".to_string(),
            format: SampleFormat::new(48_000, 16, 2),
            options: String::new(),
            #[cfg(feature = "encryption")]
            encryption_psk: None,
        };
        let err = match create(&cfg) {
            Ok(_) => panic!("unknown codec must fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("unsupported codec"));
    }

    #[cfg(feature = "f32lz4")]
    #[test]
    fn create_f32lz4_encoder_from_config() {
        let cfg = EncoderConfig {
            codec: snapcast_proto::CODEC_F32LZ4.to_string(),
            format: SampleFormat::new(48_000, 32, 2),
            options: String::new(),
            #[cfg(feature = "encryption")]
            encryption_psk: None,
        };
        let enc = create(&cfg).expect("f32lz4 encoder should be created");
        assert_eq!(enc.name(), snapcast_proto::CODEC_F32LZ4);
    }

    #[cfg(all(feature = "f32lz4", feature = "encryption"))]
    #[test]
    fn create_f32lz4_with_psk_sets_encrypted_header() {
        let cfg = EncoderConfig {
            codec: snapcast_proto::CODEC_F32LZ4.to_string(),
            format: SampleFormat::new(48_000, 32, 2),
            options: String::new(),
            encryption_psk: Some("test-key".to_string()),
        };
        let enc = create(&cfg).expect("encrypted f32lz4 encoder should be created");
        assert_eq!(enc.name(), snapcast_proto::CODEC_F32LZ4);
        assert_eq!(
            enc.header().len(),
            snapcast_proto::f32lz4::F32LZ4_ENC_HEADER_LEN
        );
    }

    #[test]
    fn f32_to_24_bit_pcm_uses_padded_samples() {
        let pcm = f32_to_pcm(&[0.0, 1.0, -1.0], 24).unwrap();
        assert_eq!(pcm.len(), 12);
        assert_eq!(pcm_to_f32(&pcm, 24).unwrap().len(), 3);
    }

    #[test]
    fn f32_to_16_bit_pcm_uses_two_byte_samples() {
        let pcm = f32_to_pcm(&[0.0, 1.0], 16).unwrap();
        assert_eq!(pcm.len(), 4);
    }

    #[test]
    fn unsupported_bit_depths_return_errors() {
        assert!(f32_to_pcm(&[0.0], 20).is_err());
        assert!(pcm_to_f32(&[0, 0], 20).is_err());
    }

    #[test]
    fn pcm_to_f32_ignores_trailing_partial_sample_bytes() {
        // 16-bit path: one full sample + 1 trailing byte.
        let pcm16 = [0x34, 0x12, 0xFF];
        let out16 = pcm_to_f32(&pcm16, 16).unwrap();
        assert_eq!(out16.len(), 1);

        // 24-bit-packed-as-i32 path: one full sample + 1 trailing byte.
        let pcm24 = [0x01, 0x02, 0x03, 0x04, 0xFF];
        let out24 = pcm_to_f32(&pcm24, 24).unwrap();
        assert_eq!(out24.len(), 1);
    }

    #[test]
    fn conversion_clamps_to_unit_interval_on_round_trip() {
        let samples = [-2.0_f32, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0];
        let pcm16 = f32_to_pcm(&samples, 16).unwrap();
        let back16 = pcm_to_f32(&pcm16, 16).unwrap();
        assert_eq!(back16.len(), samples.len());
        for &v in &back16 {
            assert!((-1.1..=1.1).contains(&v));
        }

        let pcm32 = f32_to_pcm(&samples, 32).unwrap();
        let back32 = pcm_to_f32(&pcm32, 32).unwrap();
        assert_eq!(back32.len(), samples.len());
        for &v in &back32 {
            assert!((-1.1..=1.1).contains(&v));
        }
    }
}
