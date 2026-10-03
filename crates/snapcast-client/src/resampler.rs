//! Sample rate conversion using rubato.
//!
//! Activated when the server sample rate differs from the player sample rate.

use anyhow::{Result, bail};
use rubato::{FftFixedIn, Resampler as RubatoResampler};
use snapcast_proto::SampleFormat;

use crate::stream::SampleEncoding;

/// Resampler that converts between sample rates.
pub struct Resampler {
    resampler: FftFixedIn<f64>,
    in_format: SampleFormat,
    in_encoding: SampleEncoding,
    out_format: SampleFormat,
    channels: usize,
}

impl Resampler {
    /// Create a new resampler. Returns `None` if no resampling is needed.
    pub fn new_if_needed(
        in_format: SampleFormat,
        out_format: SampleFormat,
        in_encoding: SampleEncoding,
        chunk_frames: usize,
    ) -> Result<Option<Self>> {
        if out_format.rate() == 0 || out_format.rate() == in_format.rate() {
            return Ok(None);
        }

        let channels = in_format.channels() as usize;
        if channels == 0 {
            bail!("cannot resample 0 channels");
        }

        let resampler = FftFixedIn::new(
            in_format.rate() as usize,
            out_format.rate() as usize,
            chunk_frames,
            2, // sub-chunks
            channels,
        )?;

        Ok(Some(Self {
            resampler,
            in_format,
            in_encoding,
            out_format,
            channels,
        }))
    }

    /// Resample interleaved sample data in-place.
    pub fn process(&mut self, data: &mut Vec<u8>) -> Result<()> {
        let sample_size = self.in_format.sample_size() as usize;
        let frame_size = self.in_format.frame_size() as usize;
        if frame_size == 0 || sample_size == 0 {
            bail!("cannot resample zero-sized frames");
        }
        let in_frames = data.len() / frame_size;

        // Deinterleave to f64 channels
        let mut channels_in: Vec<Vec<f64>> = vec![vec![0.0; in_frames]; self.channels];
        for (frame_idx, frame_bytes) in data.chunks_exact(frame_size).enumerate() {
            for (ch, sample_bytes) in frame_bytes.chunks_exact(sample_size).enumerate() {
                let sample = match sample_size {
                    2 => {
                        i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]) as f64
                            / i16::MAX as f64
                    }
                    4 if self.in_encoding == SampleEncoding::Float32 => f32::from_le_bytes([
                        sample_bytes[0],
                        sample_bytes[1],
                        sample_bytes[2],
                        sample_bytes[3],
                    ])
                        as f64,
                    4 if self.in_format.bits() == 24 => {
                        i32::from_le_bytes([
                            sample_bytes[0],
                            sample_bytes[1],
                            sample_bytes[2],
                            sample_bytes[3],
                        ]) as f64
                            / snapcast_proto::PCM_24BIT_MAX as f64
                    }
                    4 => {
                        i32::from_le_bytes([
                            sample_bytes[0],
                            sample_bytes[1],
                            sample_bytes[2],
                            sample_bytes[3],
                        ]) as f64
                            / i32::MAX as f64
                    }
                    _ => 0.0,
                };
                channels_in[ch][frame_idx] = sample;
            }
        }

        let channels_out = self.resampler.process(&channels_in, None)?;
        let out_frames = channels_out[0].len();
        let mut out = Vec::with_capacity(out_frames * self.channels * 4);

        for frame_idx in 0..out_frames {
            for ch_samples in &channels_out {
                let s = ch_samples[frame_idx] as f32;
                out.extend_from_slice(&s.to_le_bytes());
            }
        }

        *data = out;
        Ok(())
    }

    /// Output encoding is always f32 (rubato works in f64 internally).
    pub fn output_encoding(&self) -> SampleEncoding {
        SampleEncoding::Float32
    }

    /// Output format: same channels as input, 32-bit, at the target rate.
    pub fn output_format(&self) -> SampleFormat {
        SampleFormat::new(self.out_format.rate(), 32, self.in_format.channels())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_resampler(
        in_fmt: SampleFormat,
        out_fmt: SampleFormat,
        enc: SampleEncoding,
        frames: usize,
    ) -> Resampler {
        Resampler::new_if_needed(in_fmt, out_fmt, enc, frames)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn no_resampler_when_same_rate() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let r = Resampler::new_if_needed(fmt, fmt, SampleEncoding::PcmInt, 480).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn no_resampler_when_out_rate_zero() {
        let in_fmt = SampleFormat::new(48000, 16, 2);
        let out_fmt = SampleFormat::new(0, 16, 2);
        let r = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, 480).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn creates_resampler_for_different_rates() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let r = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, 441).unwrap();
        assert!(r.is_some());
    }

    #[test]
    fn zero_channels_rejected() {
        let in_fmt = SampleFormat::new(44100, 16, 0);
        let out_fmt = SampleFormat::new(48000, 16, 0);
        let err = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, 441)
            .err()
            .expect("expected zero-channel error");
        assert!(err.to_string().contains("cannot resample 0 channels"));
    }

    #[test]
    fn output_contract_is_f32_and_target_rate() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 7); // channels are intentionally ignored
        let r = make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, 441);
        assert_eq!(r.output_encoding(), SampleEncoding::Float32);
        let sf = r.output_format();
        assert_eq!(sf.rate(), 48000);
        assert_eq!(sf.bits(), 32);
        assert_eq!(sf.channels(), 2);
    }

    #[test]
    fn resample_changes_length() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let frames = 441; // 10ms at 44100
        let mut r = make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, frames);

        let in_bytes = frames * in_fmt.frame_size() as usize;
        let mut data = vec![0u8; in_bytes];
        // Fill with a simple pattern
        for (i, chunk) in data.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let sample = ((i as f64 * 0.1).sin() * 10000.0) as i16;
            chunk.copy_from_slice(&sample.to_le_bytes());
        }

        r.process(&mut data).unwrap();

        // Output is non-empty and differs from input length
        // (rubato FFT resampler has latency, first call produces fewer frames)
        assert!(!data.is_empty());
        assert_ne!(data.len(), in_bytes);
    }

    #[test]
    fn process_zero_sized_frames_errors() {
        let in_fmt = SampleFormat::new(44100, 0, 2); // sample_size == 0
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let mut r = make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, 441);
        let mut data = vec![0u8; 16];
        let err = r.process(&mut data).unwrap_err();
        assert!(err.to_string().contains("cannot resample zero-sized frames"));
    }

    #[test]
    fn process_float32_input_path() {
        let in_fmt = SampleFormat::new(44100, 32, 2);
        let out_fmt = SampleFormat::new(48000, 32, 2);
        let frames = 441;
        let mut r = make_resampler(in_fmt, out_fmt, SampleEncoding::Float32, frames);

        let mut data = Vec::with_capacity(frames * in_fmt.frame_size() as usize);
        for i in 0..(frames * in_fmt.channels() as usize) {
            let s = ((i as f32) * 0.01).sin();
            data.extend_from_slice(&s.to_le_bytes());
        }
        r.process(&mut data).unwrap();
        assert!(!data.is_empty());
        assert_eq!(data.len() % (in_fmt.channels() as usize * 4), 0);
    }

    #[test]
    fn process_24bit_packed_in_i32_path() {
        let in_fmt = SampleFormat::new(44100, 24, 2);
        let out_fmt = SampleFormat::new(48000, 24, 2);
        let frames = 441;
        let mut r = make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, frames);

        let mut data = Vec::with_capacity(frames * in_fmt.frame_size() as usize);
        for i in 0..(frames * in_fmt.channels() as usize) {
            let s = ((i as i32 % 1000) - 500) * 1000;
            data.extend_from_slice(&s.to_le_bytes());
        }
        r.process(&mut data).unwrap();
        assert!(!data.is_empty());
        assert_eq!(data.len() % (in_fmt.channels() as usize * 4), 0);
    }

    #[test]
    fn process_ignores_trailing_partial_frame_bytes() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let frames = 441;
        let mut r = make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, frames);

        let mut data = vec![0u8; frames * in_fmt.frame_size() as usize + 1];
        r.process(&mut data).unwrap();
        assert!(!data.is_empty());
        assert_eq!(data.len() % (in_fmt.channels() as usize * 4), 0);
    }
}
