//! Sample rate conversion using rubato.
//!
//! Activated when the server sample rate differs from the player sample rate.

use anyhow::{Result, ensure};
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
    /// Create a fixed-input resampler. Returns `None` if the target rate is zero
    /// or matches the input rate. Only the rate is used from `out_format`.
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
        ensure!(channels != 0, "cannot resample 0 channels");
        ensure!(
            chunk_frames >= 2,
            "resampling requires at least 2 frames per block"
        );
        ensure!(
            matches!(
                (in_encoding, in_format.bits()),
                (SampleEncoding::PcmInt, 16 | 24 | 32) | (SampleEncoding::Float32, 32)
            ),
            "unsupported resampler input: {:?} with {} bits",
            in_encoding,
            in_format.bits()
        );

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

    /// Number of complete input frames required by each call to [`Self::process`].
    pub fn input_frames(&self) -> usize {
        self.resampler.input_frames_next()
    }

    /// Filter delay in output frames.
    pub fn output_delay(&self) -> usize {
        self.resampler.output_delay()
    }

    /// Resample one fixed-size block of interleaved samples in-place.
    ///
    /// Trailing partial-frame bytes are ignored. An incorrect number of
    /// complete frames returns an error without consuming any input.
    /// Output is interleaved little-endian f32 and may be empty while an FFT
    /// block accumulates.
    pub fn process(&mut self, data: &mut Vec<u8>) -> Result<()> {
        let sample_size = usize::from(self.in_format.sample_size());
        let frame_size = self.channels * sample_size;
        let in_frames = data.len() / frame_size;
        let expected = self.input_frames();
        ensure!(
            in_frames == expected,
            "resampler needs {expected} input frames, got {in_frames}"
        );

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
                    _ => unreachable!("input format validated at construction"),
                };
                channels_in[ch][frame_idx] = sample;
            }
        }

        let channels_out = self.resampler.process(&channels_in, None)?;
        let out_frames = channels_out[0].len();
        data.clear();
        data.reserve(out_frames * self.channels * 4);

        for frame_idx in 0..out_frames {
            for ch_samples in &channels_out {
                let s = ch_samples[frame_idx] as f32;
                data.extend_from_slice(&s.to_le_bytes());
            }
        }

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
    fn resample_preserves_frame_ratio_across_blocks() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let frames = 441; // 10ms at 44100
        let mut r = make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, frames);

        let mut total_frames = 0;
        for _ in 0..4 {
            let mut data = vec![0; frames * usize::from(in_fmt.frame_size())];
            r.process(&mut data).unwrap();
            assert_eq!(data.len() % 8, 0);
            total_frames += data.len() / 8;
        }
        assert_eq!(total_frames, 4 * 480);
    }

    #[test]
    fn unsupported_input_formats_are_rejected() {
        let out_fmt = SampleFormat::new(48000, 16, 2);
        for (bits, encoding) in [
            (0, SampleEncoding::PcmInt),
            (8, SampleEncoding::PcmInt),
            (16, SampleEncoding::Float32),
            (24, SampleEncoding::Float32),
        ] {
            let err =
                Resampler::new_if_needed(SampleFormat::new(44100, bits, 2), out_fmt, encoding, 441)
                    .err()
                    .expect("unsupported format");
            assert!(err.to_string().contains("unsupported resampler input"));
        }
    }

    #[test]
    fn tiny_input_blocks_are_rejected() {
        let input = SampleFormat::new(44100, 16, 2);
        let output = SampleFormat::new(48000, 16, 2);
        for size in [0, 1] {
            let err = Resampler::new_if_needed(input, output, SampleEncoding::PcmInt, size)
                .err()
                .expect("block too small");
            assert!(err.to_string().contains("at least 2 frames"));
        }
    }

    #[test]
    fn incorrect_complete_frame_counts_leave_input_unchanged() {
        let input = SampleFormat::new(44100, 16, 2);
        let output = SampleFormat::new(48000, 16, 2);
        let mut r = make_resampler(input, output, SampleEncoding::PcmInt, 441);
        for frames in [0, 440, 442] {
            let mut data = vec![7; frames * 4];
            assert!(r.process(&mut data).is_err());
            assert_eq!(data, vec![7; frames * 4]);
        }
    }

    #[test]
    fn resample_preserves_stereo_amplitude_for_each_encoding() {
        let output = SampleFormat::new(48000, 32, 2);
        for (bits, encoding, frame) in [
            (
                16,
                SampleEncoding::PcmInt,
                [8192_i16.to_le_bytes(), (-16384_i16).to_le_bytes()].concat(),
            ),
            (
                24,
                SampleEncoding::PcmInt,
                [2097152_i32.to_le_bytes(), (-4194304_i32).to_le_bytes()].concat(),
            ),
            (
                32,
                SampleEncoding::PcmInt,
                [536870912_i32.to_le_bytes(), (-1073741824_i32).to_le_bytes()].concat(),
            ),
            (
                32,
                SampleEncoding::Float32,
                [0.25_f32.to_le_bytes(), (-0.5_f32).to_le_bytes()].concat(),
            ),
        ] {
            let input = SampleFormat::new(44100, bits, 2);
            let mut r = make_resampler(input, output, encoding, 441);
            let mut data = frame.repeat(441);
            r.process(&mut data).unwrap();
            data = frame.repeat(441);
            r.process(&mut data).unwrap();
            let (frames, rest) = data.as_chunks::<8>();
            assert!(rest.is_empty() && !frames.is_empty());
            for stereo in frames {
                let (left, right) = stereo.split_at(4);
                assert!((f32::from_le_bytes(left.try_into().unwrap()) - 0.25).abs() < 0.001);
                assert!((f32::from_le_bytes(right.try_into().unwrap()) + 0.5).abs() < 0.001);
            }
        }
    }

    #[test]
    fn process_ignores_trailing_partial_frame_bytes() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let frames = 441;
        let mut r = make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, frames);

        let mut exact = vec![0u8; frames * in_fmt.frame_size() as usize];
        let mut data = exact.clone();
        data.push(0xff);
        make_resampler(in_fmt, out_fmt, SampleEncoding::PcmInt, frames)
            .process(&mut exact)
            .unwrap();
        r.process(&mut data).unwrap();
        assert_eq!(data, exact);
    }
}
