//! Audio output — cpal callback reads from Stream directly.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use snapcast_client::AudioFrame;
use snapcast_client::connection::now_usec;
use snapcast_client::stream::{SampleEncoding, Stream};
use snapcast_client::time_provider::TimeProvider;
use tokio::sync::{mpsc, watch};

use crate::mixer::VolumeState;

/// Start audio output once the stream has a valid format. Dropping the stop
/// sender stops playback, including any active blocking audio worker.
pub async fn play_audio(
    rx: mpsc::Receiver<AudioFrame>,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Arc<VolumeState>,
    mut stop: watch::Receiver<()>,
) {
    tokio::spawn(async move {
        let mut rx = rx;
        while rx.recv().await.is_some() {}
    });

    loop {
        let (format, encoding) = loop {
            if stop.has_changed().is_err() {
                return;
            }
            {
                let s = stream.lock().unwrap_or_else(|e| e.into_inner());
                let f = s.format();
                if f.rate() > 0 && f.channels() > 0 {
                    break (f, s.encoding());
                }
            }
            tokio::select! {
                _ = stop.changed() => return,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        };

        tracing::info!(
            rate = format.rate(),
            bits = format.bits(),
            channels = format.channels(),
            "Audio format detected"
        );

        let stream_clone = Arc::clone(&stream);
        let tp_clone = Arc::clone(&time_provider);
        let vol_clone = Arc::clone(&volume);
        let worker_stop = stop.clone();
        let result = tokio::task::spawn_blocking(move || {
            run_cpal(
                stream_clone,
                tp_clone,
                format,
                encoding,
                vol_clone,
                worker_stop,
            )
        })
        .await;
        if stop.has_changed().is_err() {
            return;
        }
        match result {
            Ok(Ok(())) => {
                tracing::info!("Audio format change detected, restarting player");
                continue;
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "Audio output failed, retrying in 1s");
            }
            Err(e) => {
                tracing::error!(error = %e, "Task join failed, restarting in 1s");
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

fn select_output_config(
    configs: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
    format: snapcast_proto::SampleFormat,
    allow_resampling: bool,
) -> Option<cpal::SupportedStreamConfig> {
    configs
        .into_iter()
        .filter(|config| {
            config.channels() == format.channels()
                && config.sample_format() == cpal::SampleFormat::F32
        })
        .filter_map(|config| {
            let rate = if allow_resampling {
                format
                    .rate()
                    .clamp(config.min_sample_rate(), config.max_sample_rate())
            } else {
                format.rate()
            };
            config.try_with_sample_rate(rate)
        })
        .min_by_key(|config| config.sample_rate().abs_diff(format.rate()))
}

fn run_cpal(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: snapcast_proto::SampleFormat,
    encoding: SampleEncoding,
    volume: Arc<VolumeState>,
    stop: watch::Receiver<()>,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

    match device.description() {
        Ok(description) => tracing::info!(device = %description.name(), "Using audio device"),
        Err(error) => tracing::warn!(%error, "Could not read audio device description"),
    }

    let allow_resampling = cfg!(feature = "resampler");
    let config = select_output_config(device.supported_output_configs()?, format, allow_resampling)
        .ok_or_else(|| {
            let requirement = if allow_resampling {
                "at any sample rate (channel conversion is not supported)"
            } else {
                "at the stream sample rate (resampler feature is disabled)"
            };
            anyhow::anyhow!(
                "no compatible f32 output for {} Hz/{} ch: device must support f32 with {} channels {requirement}",
                format.rate(),
                format.channels(),
                format.channels()
            )
        })?
        .config();

    let device_rate = config.sample_rate;
    let device_channels = usize::from(config.channels);

    #[cfg(feature = "resampler")]
    let mut resampler = if device_rate != format.rate() {
        tracing::warn!(
            input_rate = format.rate(),
            output_rate = device_rate,
            "Resampling audio"
        );
        let device_format = snapcast_proto::SampleFormat::new(device_rate, 32, config.channels);
        PlaybackResampler::new(format, device_format, encoding)?
    } else {
        None
    };

    let stream_cb = Arc::clone(&stream);
    let tp_cb = Arc::clone(&time_provider);
    // Reused across callbacks and resized in place to minimize heap activity in
    // the realtime audio path. Allocating inside a cpal callback can stall the
    // audio thread and cause xruns/glitches.
    let mut pcm_buf: Vec<u8> = Vec::new();
    let cpal_stream = device.build_output_stream(
        config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let num_frames = data.len() / device_channels;

            let timestamp = info.timestamp();
            let buffer_dac_usec = timestamp
                .playback
                .duration_since(timestamp.callback)
                .as_micros() as i64;

            let server_now = {
                let tp = tp_cb.lock().unwrap_or_else(|e| e.into_inner());
                now_usec() + tp.diff_to_server_usec()
            };

            let mut s = stream_cb.lock().unwrap_or_else(|e| e.into_inner());
            let current_format = s.format();
            let current_encoding = s.encoding();

            // Format change detection
            if current_format != format || current_encoding != encoding {
                // Return silence and hope the main loop picks up the change
                data.fill(0.0);
                return;
            }

            let frame_size = current_format.frame_size() as usize;
            if frame_size == 0 {
                data.fill(0.0);
                return;
            }

            #[cfg(feature = "resampler")]
            if let Some(ref mut r) = resampler {
                drop(s);
                if let Err(e) = r.fill(data, |pcm, frames, offset_usec| {
                    pcm.fill(0);
                    let mut s = stream_cb.lock().unwrap_or_else(|e| e.into_inner());
                    if s.format() == format && s.encoding() == encoding {
                        s.get_player_chunk_or_silence(
                            server_now,
                            buffer_dac_usec + offset_usec,
                            pcm,
                            frames,
                        );
                    }
                }) {
                    tracing::error!(error = %e, "Resampling failed");
                    data.fill(0.0);
                    return;
                }
            } else {
                pcm_buf.resize(num_frames * frame_size, 0);
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec + (num_frames as i64 * 1_000_000) / i64::from(device_rate),
                    &mut pcm_buf,
                    num_frames as u32,
                );
                drop(s);

                write_samples_to_output(data, &pcm_buf, current_format, current_encoding);
            }
            #[cfg(not(feature = "resampler"))]
            {
                pcm_buf.resize(num_frames * frame_size, 0);
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec + (num_frames as i64 * 1_000_000) / i64::from(device_rate),
                    &mut pcm_buf,
                    num_frames as u32,
                );
                drop(s);

                write_samples_to_output(data, &pcm_buf, current_format, current_encoding);
            }

            // Apply software volume
            apply_software_volume(data, volume.gain());
        },
        |err| tracing::error!(error = %err, "Audio stream error"),
        None,
    )?;

    run_output(
        &stop,
        || Ok(cpal_stream.play()?),
        || {
            tracing::info!("Audio playback started");
            #[cfg(target_os = "linux")]
            crate::notify_status(&format!("Playing {device_rate} Hz/{device_channels} ch"));
        },
        || {
            let s = stream.lock().unwrap_or_else(|e| e.into_inner());
            s.format() == format && s.encoding() == encoding
        },
    )
}

fn run_output(
    stop: &watch::Receiver<()>,
    play: impl FnOnce() -> anyhow::Result<()>,
    started: impl FnOnce(),
    format_matches: impl Fn() -> bool,
) -> anyhow::Result<()> {
    if stop.has_changed().is_ok() {
        play()?;
        started();
        while stop.has_changed().is_ok() && format_matches() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

#[cfg(feature = "resampler")]
struct PlaybackResampler {
    inner: snapcast_client::resampler::Resampler,
    input: snapcast_proto::SampleFormat,
    block: Vec<u8>,
    offset: usize,
    // Input not yet represented in output, in units of 1 / (input_rate * output_rate) seconds.
    pending_ticks: i64,
}

#[cfg(feature = "resampler")]
impl PlaybackResampler {
    fn new(
        input: snapcast_proto::SampleFormat,
        output: snapcast_proto::SampleFormat,
        encoding: SampleEncoding,
    ) -> anyhow::Result<Option<Self>> {
        anyhow::ensure!(
            input.channels() == output.channels(),
            "cannot play {} input channels on a {}-channel output without channel conversion",
            input.channels(),
            output.channels()
        );
        let frames = (input.rate() / 50).max(2) as usize;
        Ok(
            snapcast_client::resampler::Resampler::new_if_needed(input, output, encoding, frames)?
                .map(|inner| Self {
                    inner,
                    input,
                    block: Vec::new(),
                    offset: 0,
                    pending_ticks: 0,
                }),
        )
    }

    fn fill(
        &mut self,
        output: &mut [f32],
        mut read_input: impl FnMut(&mut [u8], u32, i64),
    ) -> anyhow::Result<()> {
        let channels = usize::from(self.input.channels());
        let in_rate = i64::from(self.input.rate());
        let out_rate = i64::from(self.inner.output_format().rate());
        let delay = self.inner.output_delay() as i64;
        let frames = self.inner.input_frames() as u32;
        let mut written = 0;
        while written < output.len() {
            if self.offset == self.block.len() {
                let output_ahead = (written / channels) as i64 + delay;
                let offset_ticks = output_ahead * in_rate + self.pending_ticks;
                let offset_usec = (i128::from(offset_ticks) * 1_000_000
                    / (i128::from(in_rate) * i128::from(out_rate)))
                    as i64;
                self.block
                    .resize(frames as usize * usize::from(self.input.frame_size()), 0);
                read_input(&mut self.block, frames, offset_usec);
                self.inner.process(&mut self.block)?;
                self.offset = 0;
                let produced = (self.block.len() / (channels * 4)) as i64;
                self.pending_ticks += i64::from(frames) * out_rate - produced * in_rate;
            }

            let available = (self.block.len() - self.offset) / 4;
            let count = available.min(output.len() - written);
            let end = self.offset + count * 4;
            for (dst, src) in output[written..written + count]
                .iter_mut()
                .zip(self.block[self.offset..end].as_chunks::<4>().0)
            {
                *dst = f32::from_le_bytes(*src);
            }
            self.offset = end;
            written += count;
        }
        Ok(())
    }
}

#[inline]
fn apply_software_volume(output: &mut [f32], gain: f32) {
    let gain = gain.clamp(0.0, 1.0);
    if gain == 1.0 {
        return;
    }
    for sample in output.iter_mut() {
        *sample *= gain;
    }
}

fn write_samples_to_output(
    output: &mut [f32],
    samples: &[u8],
    format: snapcast_proto::SampleFormat,
    encoding: SampleEncoding,
) {
    output.fill(0.0);
    match encoding {
        SampleEncoding::Float32 => {
            for (i, chunk) in samples
                .as_chunks::<4>()
                .0
                .iter()
                .take(output.len())
                .enumerate()
            {
                output[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
        }
        SampleEncoding::PcmInt => match format.bits() {
            16 => {
                for (i, chunk) in samples
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take(output.len())
                    .enumerate()
                {
                    output[i] = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32;
                }
            }
            24 => {
                for (i, chunk) in samples
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .take(output.len())
                    .enumerate()
                {
                    output[i] = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32
                        / snapcast_proto::PCM_24BIT_MAX;
                }
            }
            32 => {
                for (i, chunk) in samples
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .take(output.len())
                    .enumerate()
                {
                    output[i] = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32
                        / i32::MAX as f32;
                }
            }
            _ => output.fill(0.0),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn active_output_worker_exits_when_stopped() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (stop, receiver) = watch::channel(());
        let (started, output_started) = tokio::sync::oneshot::channel();
        let format_matches = Arc::new(AtomicBool::new(true));
        let worker_format = Arc::clone(&format_matches);
        let worker = tokio::task::spawn_blocking(move || {
            run_output(
                &receiver,
                || Ok(()),
                || {
                    let _ = started.send(());
                },
                || worker_format.load(Ordering::Relaxed),
            )
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let started = tokio::time::timeout_at(deadline, output_started).await;
        drop(stop);
        let stopped = tokio::time::timeout_at(deadline, worker).await;
        format_matches.store(false, Ordering::Relaxed);
        started.unwrap().unwrap();
        stopped
            .expect("audio worker should exit even though its format has not changed")
            .unwrap()
            .unwrap();
    }

    #[test]
    fn failed_output_does_not_report_playback_started() {
        let (_stop, receiver) = watch::channel(());
        let error = run_output(
            &receiver,
            || anyhow::bail!("cannot start output"),
            || panic!("failed output must not report playback"),
            || false,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "cannot start output");
    }

    fn output_range(
        channels: u16,
        min_rate: u32,
        max_rate: u32,
        sample_format: cpal::SampleFormat,
    ) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            channels,
            min_rate,
            max_rate,
            cpal::SupportedBufferSize::Unknown,
            sample_format,
        )
    }

    #[test]
    fn output_selection_prefers_exact_f32_rate_with_matching_channels() {
        use cpal::SampleFormat::{F32, I16};

        let format = snapcast_proto::SampleFormat::new(48_000, 16, 2);
        let config = select_output_config(
            [
                output_range(2, 48_000, 48_000, I16),
                output_range(1, 48_000, 48_000, F32),
                output_range(2, 44_100, 44_100, F32),
                output_range(2, 44_100, 48_000, F32),
            ],
            format,
            true,
        )
        .unwrap();
        assert_eq!(
            (
                config.sample_format(),
                config.channels(),
                config.sample_rate()
            ),
            (F32, 2, 48_000)
        );
    }

    #[test]
    fn output_selection_only_changes_rate_when_resampling_is_allowed() {
        use cpal::SampleFormat::{F32, I16};

        let format = snapcast_proto::SampleFormat::new(48_000, 16, 2);
        let configs = [
            output_range(2, 48_000, 48_000, I16),
            output_range(1, 48_000, 48_000, F32),
            output_range(2, 96_000, 96_000, F32),
            output_range(2, 32_000, 44_100, F32),
        ];
        assert!(select_output_config(configs, format, false).is_none());
        assert!(select_output_config([configs[0], configs[1]], format, true).is_none());
        let config = select_output_config(configs, format, true).unwrap();
        assert_eq!(
            (
                config.sample_format(),
                config.channels(),
                config.sample_rate()
            ),
            (F32, 2, 44_100)
        );
    }

    #[test]
    fn apply_software_volume_scales_and_clamps() {
        let mut out = [0.5, -0.5];
        apply_software_volume(&mut out, 0.5);
        assert_eq!(out, [0.25, -0.25]);

        apply_software_volume(&mut out, 2.0);
        assert_eq!(out, [0.25, -0.25]);

        apply_software_volume(&mut out, -1.0);
        assert_eq!(out, [0.0, -0.0]);
    }

    #[test]
    fn write_float32_samples_to_output() {
        let fmt = snapcast_proto::SampleFormat::new(48_000, 32, 2);
        let input: Vec<u8> = [0.25f32, -0.5f32]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let mut out = [0.0; 2];
        write_samples_to_output(&mut out, &input, fmt, SampleEncoding::Float32);
        assert!((out[0] - 0.25).abs() < 1e-6);
        assert!((out[1] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn write_pcm16_samples_to_output() {
        let fmt = snapcast_proto::SampleFormat::new(48_000, 16, 2);
        let input: Vec<u8> = [i16::MAX, i16::MIN]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        let mut out = [0.0; 2];
        write_samples_to_output(&mut out, &input, fmt, SampleEncoding::PcmInt);
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert!((out[1] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn unsupported_pcm_bit_depth_outputs_silence() {
        let fmt = snapcast_proto::SampleFormat::new(48_000, 20, 2);
        let input = vec![0xFF; 8];
        let mut out = [0.1, -0.2];
        write_samples_to_output(&mut out, &input, fmt, SampleEncoding::PcmInt);
        assert_eq!(out, [0.0, 0.0]);
    }

    #[cfg(feature = "resampler")]
    #[test]
    fn playback_resampler_rejects_channel_mismatch_even_at_same_rate() {
        let input = snapcast_proto::SampleFormat::new(44_100, 16, 1);
        for rate in [44_100, 48_000] {
            let output = snapcast_proto::SampleFormat::new(rate, 32, 2);
            let err = PlaybackResampler::new(input, output, SampleEncoding::PcmInt)
                .err()
                .expect("channel mismatch must be rejected");
            assert!(err.to_string().contains("without channel conversion"));
        }
    }

    #[cfg(feature = "resampler")]
    #[test]
    fn variable_callbacks_preserve_ordered_resampler_output() {
        use snapcast_client::resampler::Resampler;
        use snapcast_proto::SampleFormat;

        let input = SampleFormat::new(44_100, 32, 2);
        let output = SampleFormat::new(48_000, 32, 2);
        let mut player = PlaybackResampler::new(input, output, SampleEncoding::Float32)
            .unwrap()
            .unwrap();
        let mut reference = Resampler::new_if_needed(input, output, SampleEncoding::Float32, 882)
            .unwrap()
            .unwrap();
        let source: Vec<u8> = (0..2_646)
            .flat_map(|i| [(i as f32 * 0.03).sin(), -(i as f32 * 0.07).cos()])
            .flat_map(f32::to_le_bytes)
            .collect();
        let expected: Vec<f32> = source
            .as_chunks::<{ 882 * 8 }>()
            .0
            .iter()
            .flat_map(|chunk| {
                let mut block = chunk.to_vec();
                reference.process(&mut block).unwrap();
                block
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .copied()
                    .map(f32::from_le_bytes)
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(expected.len(), 2_880 * 2);

        let mut cursor = 0;
        let mut actual = Vec::new();
        for frames in [128, 511, 73, 1_024, 1_144] {
            let mut callback = vec![f32::NAN; frames * 2];
            player
                .fill(&mut callback, |pcm, _, _| {
                    let end = cursor + pcm.len();
                    pcm.copy_from_slice(&source[cursor..end]);
                    cursor = end;
                })
                .unwrap();
            actual.extend(callback);
        }
        assert_eq!(cursor, source.len());
        assert_eq!(actual, expected);
    }
}
