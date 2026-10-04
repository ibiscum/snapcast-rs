//! Vorbis encoder using vorbis_rs (libvorbis bindings).

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;
use vorbis_rs::VorbisEncoderBuilder;

use super::{EncodedChunk, Encoder};
use crate::AudioData;

/// Vorbis (Ogg) encoder wrapping libvorbis via vorbis_rs.
pub struct VorbisEncoder {
    format: SampleFormat,
    header: Vec<u8>,
    warned: bool,
}

impl VorbisEncoder {
    /// Create a new Vorbis encoder. Options: not yet used.
    pub fn new(format: SampleFormat, _options: &str) -> Result<Self> {
        // Build header by creating a temporary encoder and capturing the Ogg header pages
        let mut header_buf = Cursor::new(Vec::new());
        let channels_u8 = u8::try_from(format.channels())
            .map_err(|_| anyhow::anyhow!("channels must fit in u8"))?;
        let channels =
            NonZeroU8::new(channels_u8).ok_or_else(|| anyhow::anyhow!("channels must be > 0"))?;
        let rate = NonZeroU32::new(format.rate())
            .ok_or_else(|| anyhow::anyhow!("sample rate must be > 0"))?;

        let enc = VorbisEncoderBuilder::new(rate, channels, &mut header_buf)?.build()?;
        // Finish immediately to flush header pages
        enc.finish()?;

        let header = header_buf.into_inner();

        Ok(Self {
            format,
            header,
            warned: false,
        })
    }
}

impl Encoder for VorbisEncoder {
    fn name(&self) -> &str {
        snapcast_proto::CODEC_OGG
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let channels = self.format.channels() as usize;
        if channels == 0 {
            bail!("channels must be > 0");
        }
        let channels_u8 = u8::try_from(channels).map_err(|_| anyhow::anyhow!("channels must fit in u8"))?;

        // Build per-channel f32 buffers
        let channel_bufs: Vec<Vec<f32>> = match input {
            AudioData::F32(samples) => {
                // Deinterleave f32 directly — zero conversion.
                // Drop trailing partial frame to keep channel buffers equal.
                let frames = samples.len() / channels;
                let mut bufs = vec![Vec::with_capacity(frames); channels];
                for (i, &s) in samples[..frames * channels].iter().enumerate() {
                    bufs[i % channels].push(s);
                }
                bufs
            }
            AudioData::Pcm(pcm) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "vorbis",
                        "PCM input requires scaling to f32 — consider sending F32 directly"
                    );
                }
                let sample_size = self.format.sample_size() as usize;
                if sample_size == 0 {
                    bail!("invalid sample size: 0 bytes");
                }
                let frame_size = sample_size * channels;
                let aligned = pcm.len() - pcm.len() % frame_size.max(1);
                let pcm = &pcm[..aligned];
                let total_samples = pcm.len() / sample_size;
                let frames = total_samples / channels;
                let mut bufs = vec![Vec::with_capacity(frames); channels];
                let scale = match self.format.bits() {
                    16 => 1.0 / 32768.0,
                    24 => 1.0 / snapcast_proto::PCM_24BIT_MAX,
                    32 => 1.0 / 2_147_483_648.0,
                    _ => bail!("unsupported sample size: {sample_size}"),
                };
                match sample_size {
                    2 => {
                        for (i, chunk) in pcm.as_chunks::<2>().0.iter().enumerate() {
                            let s = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 * scale;
                            bufs[i % channels].push(s);
                        }
                    }
                    4 => {
                        for (i, chunk) in pcm.as_chunks::<4>().0.iter().enumerate() {
                            let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                                as f32
                                * scale;
                            bufs[i % channels].push(s);
                        }
                    }
                    _ => unreachable!(),
                }
                bufs
            }
        };

        let channel_refs: Vec<&[f32]> = channel_bufs.iter().map(|v| v.as_slice()).collect();

        let mut output = Cursor::new(Vec::new());
        let ch = NonZeroU8::new(channels_u8).ok_or_else(|| anyhow::anyhow!("zero channels"))?;
        let rate = NonZeroU32::new(self.format.rate())
            .ok_or_else(|| anyhow::anyhow!("zero sample rate"))?;
        let mut enc = VorbisEncoderBuilder::new(rate, ch, &mut output)?.build()?;
        enc.encode_audio_block(channel_refs)?;
        enc.finish()?;

        Ok(EncodedChunk {
            data: output.into_inner(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_built_and_non_empty() {
        let enc = VorbisEncoder::new(SampleFormat::new(48_000, 16, 2), "").unwrap();
        assert!(!enc.header().is_empty());
    }

    #[test]
    fn constructor_rejects_zero_rate_or_channels() {
        assert!(VorbisEncoder::new(SampleFormat::new(0, 16, 2), "").is_err());
        assert!(VorbisEncoder::new(SampleFormat::new(48_000, 16, 0), "").is_err());
    }

    #[test]
    fn constructor_rejects_channels_out_of_u8_range() {
        let err = match VorbisEncoder::new(SampleFormat::new(48_000, 16, 300), "") {
            Ok(_) => panic!("expected channels out-of-range error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("channels must fit in u8"));
    }

    #[test]
    fn encode_f32_produces_output() {
        let mut enc = VorbisEncoder::new(SampleFormat::new(48_000, 16, 2), "").unwrap();
        let samples = vec![0.0f32; 960 * 2];
        let out = enc.encode(&AudioData::F32(samples)).unwrap();
        assert!(!out.data.is_empty());
    }

    #[test]
    fn encode_pcm_24bit_produces_output() {
        let mut enc = VorbisEncoder::new(SampleFormat::new(48_000, 24, 2), "").unwrap();
        let mut pcm = Vec::with_capacity(960 * 2 * 4);
        for i in 0..(960 * 2) {
            let v = ((i as i32 * 1337) % 8_000_000) - 4_000_000;
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        let out = enc.encode(&AudioData::Pcm(pcm)).unwrap();
        assert!(!out.data.is_empty());
    }

    #[test]
    fn unsupported_pcm_bits_return_error() {
        let mut enc = VorbisEncoder::new(SampleFormat::new(48_000, 20, 2), "").unwrap();
        let err = match enc.encode(&AudioData::Pcm(vec![0; 128])) {
            Ok(_) => panic!("expected unsupported sample-size error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("unsupported sample size"));
    }

    #[test]
    fn partial_frame_input_is_accepted() {
        let mut enc = VorbisEncoder::new(SampleFormat::new(48_000, 16, 2), "").unwrap();
        // Not divisible by frame size (4 bytes): one trailing byte is dropped.
        let pcm = vec![0u8; 4 * 100 + 1];
        let out = enc.encode(&AudioData::Pcm(pcm)).unwrap();
        assert!(!out.data.is_empty());
    }
}
