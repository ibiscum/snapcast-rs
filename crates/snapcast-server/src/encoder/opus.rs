//! Opus encoder using audiopus.

use anyhow::{Result, bail};
use audiopus::coder::Encoder as OpusEnc;
use audiopus::{Application, Channels, SampleRate};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};
use crate::AudioData;

/// Opus encoder wrapping libopus via audiopus.
pub struct OpusEncoder {
    format: SampleFormat,
    encoder: OpusEnc,
    header: Vec<u8>,
    frame_size: usize,
    warned: bool,
}

impl OpusEncoder {
    /// Create a new Opus encoder.
    ///
    /// `options` is currently accepted for interface compatibility with other
    /// encoders but is not interpreted by this implementation.
    pub fn new(format: SampleFormat, _options: &str) -> Result<Self> {
        let sample_rate = match format.rate() {
            8000 => SampleRate::Hz8000,
            12000 => SampleRate::Hz12000,
            16000 => SampleRate::Hz16000,
            24000 => SampleRate::Hz24000,
            48000 => SampleRate::Hz48000,
            r => {
                tracing::warn!(codec = "opus", sample_rate = r, "unsupported sample rate");
                bail!("Opus does not support sample rate {r}");
            }
        };
        let channels = match format.channels() {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            c => {
                tracing::warn!(codec = "opus", channels = c, "unsupported channel count");
                bail!("Opus does not support {c} channels");
            }
        };

        let encoder = OpusEnc::new(sample_rate, channels, Application::Audio)?;

        // Build OpusHead identification header
        let mut header = Vec::with_capacity(19);
        header.extend_from_slice(b"OpusHead");
        header.push(1); // version
        header.push(format.channels() as u8);
        header.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // output gain
        header.push(0); // channel mapping family

        // 20ms frame size
        let frame_size = format.rate() as usize / 50;

        Ok(Self {
            format,
            encoder,
            header,
            frame_size,
            warned: false,
        })
    }
}

impl Encoder for OpusEncoder {
    fn name(&self) -> &str {
        snapcast_proto::CODEC_OPUS
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let pcm = match input {
            AudioData::Pcm(data) if self.format.bits() == 16 => {
                std::borrow::Cow::Borrowed(data.as_slice())
            }
            AudioData::Pcm(data) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "opus",
                        bits = self.format.bits(),
                        "PCM input requires quantization to 16-bit for Opus"
                    );
                }
                let samples = super::pcm_to_f32(data, self.format.bits())?;
                std::borrow::Cow::Owned(super::f32_to_pcm(&samples, 16)?)
            }
            AudioData::F32(samples) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "opus",
                        "F32 input requires quantization to 16-bit — consider f32lz4 for lossless path"
                    );
                }
                std::borrow::Cow::Owned(super::f32_to_pcm(samples, 16)?)
            }
        };

        let channels = self.format.channels() as usize;
        let frame_samples = self.frame_size * channels;
        let frame_bytes = frame_samples * 2; // 16-bit samples
        let total_frames = pcm.len() / (channels * 2);
        tracing::trace!(
            codec = "opus",
            input_bytes = pcm.len(),
            total_frames,
            "encode"
        );

        let mut output = Vec::new();
        let mut encode_buf = [0u8; 4096];

        for chunk in pcm.chunks(frame_bytes) {
            if chunk.len() < frame_bytes {
                break;
            }
            let samples: Vec<i16> = chunk
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();

            match self.encoder.encode(&samples, &mut encode_buf) {
                Ok(len) => output.extend_from_slice(&encode_buf[..len]),
                Err(e) => {
                    tracing::warn!(codec = "opus", error = %e, "encode failed");
                    bail!("Opus encode failed: {e}");
                }
            }
        }

        Ok(EncodedChunk { data: output })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_16_frame_bytes(rate: u32, channels: u16) -> usize {
        let frame_size = rate as usize / 50; // 20 ms
        frame_size * channels as usize * 2 // i16
    }

    #[test]
    fn header_is_valid_opus_head() {
        let fmt = SampleFormat::new(48_000, 16, 2);
        let enc = OpusEncoder::new(fmt, "").unwrap();
        let h = enc.header();
        assert_eq!(h.len(), 19);
        assert_eq!(&h[..8], b"OpusHead");
        assert_eq!(h[8], 1); // version
        assert_eq!(h[9], 2); // channels
        assert_eq!(u32::from_le_bytes([h[12], h[13], h[14], h[15]]), 48_000);
        assert_eq!(h[18], 0); // mapping family
    }

    #[test]
    fn rejects_unsupported_sample_rates() {
        let err = match OpusEncoder::new(SampleFormat::new(44_100, 16, 2), "") {
            Ok(_) => panic!("44.1kHz must be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("does not support sample rate"));
    }

    #[test]
    fn rejects_unsupported_channel_counts() {
        assert!(OpusEncoder::new(SampleFormat::new(48_000, 16, 0), "").is_err());
        assert!(OpusEncoder::new(SampleFormat::new(48_000, 16, 3), "").is_err());
    }

    #[test]
    fn options_are_currently_ignored() {
        let fmt = SampleFormat::new(48_000, 16, 2);
        assert!(OpusEncoder::new(fmt, "").is_ok());
        assert!(OpusEncoder::new(fmt, "192").is_ok());
        assert!(OpusEncoder::new(fmt, "this-is-ignored").is_ok());
    }

    #[test]
    fn encode_16bit_pcm_produces_data() {
        let fmt = SampleFormat::new(48_000, 16, 2);
        let mut enc = OpusEncoder::new(fmt, "").unwrap();
        let pcm = vec![0u8; pcm_16_frame_bytes(48_000, 2)];
        let out = enc.encode(&AudioData::Pcm(pcm)).unwrap();
        assert!(!out.data.is_empty());
    }

    #[test]
    fn encode_f32_produces_data() {
        let fmt = SampleFormat::new(48_000, 16, 2);
        let mut enc = OpusEncoder::new(fmt, "").unwrap();
        let samples = vec![0.0f32; (48_000 / 50 * 2) as usize];
        let out = enc.encode(&AudioData::F32(samples)).unwrap();
        assert!(!out.data.is_empty());
    }

    #[test]
    fn encode_24bit_pcm_quantizes_and_produces_data() {
        let fmt = SampleFormat::new(48_000, 24, 2);
        let mut enc = OpusEncoder::new(fmt, "").unwrap();
        let sample_count = (48_000 / 50 * 2) as usize;
        let mut pcm = Vec::with_capacity(sample_count * 4);
        for i in 0..sample_count {
            let v = ((i as i32 * 12345) % 8_000_000) - 4_000_000;
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        let out = enc.encode(&AudioData::Pcm(pcm)).unwrap();
        assert!(!out.data.is_empty());
    }

    #[test]
    fn trailing_partial_frame_is_dropped() {
        let fmt = SampleFormat::new(48_000, 16, 2);
        let frame_bytes = pcm_16_frame_bytes(48_000, 2);

        let mut exact = vec![0u8; frame_bytes];
        for (i, b) in exact.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }

        let mut with_tail = exact.clone();
        with_tail.extend_from_slice(&[0xAA, 0xBB]);

        let mut enc_exact = OpusEncoder::new(fmt, "").unwrap();
        let out_exact = enc_exact.encode(&AudioData::Pcm(exact)).unwrap();

        let mut enc_tail = OpusEncoder::new(fmt, "").unwrap();
        let out_tail = enc_tail.encode(&AudioData::Pcm(with_tail)).unwrap();

        assert_eq!(out_tail.data, out_exact.data);
    }
}
