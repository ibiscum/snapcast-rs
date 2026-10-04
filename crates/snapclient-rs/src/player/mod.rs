//! Audio output — cpal callback reads from Stream directly.

use std::sync::{Arc, Mutex};

use snapcast_client::AudioFrame;
use snapcast_client::connection::now_usec;
use snapcast_client::stream::{SampleEncoding, Stream};
use snapcast_client::time_provider::TimeProvider;
use tokio::sync::mpsc;

use crate::mixer::VolumeState;

/// Start audio output. Waits for the Stream to have audio, then starts cpal.
pub async fn play_audio(
    rx: mpsc::Receiver<AudioFrame>,
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    volume: Arc<VolumeState>,
) {
    // Drain audio_rx in background
    tokio::spawn(async move {
        let mut rx = rx;
        while rx.recv().await.is_some() {}
    });

    loop {
        // Wait for the Stream to have a valid format
        let (format, encoding) = loop {
            {
                let s = stream.lock().unwrap_or_else(|e| e.into_inner());
                let f = s.format();
                if f.rate() > 0 && f.channels() > 0 {
                    break (f, s.encoding());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };

        tracing::info!(
            rate = format.rate(),
            bits = format.bits(),
            channels = format.channels(),
            "Audio format detected"
        );

        // Run cpal on dedicated thread.
        let stream_clone = Arc::clone(&stream);
        let tp_clone = Arc::clone(&time_provider);
        let vol_clone = Arc::clone(&volume);

        // We use spawn_blocking to wait for the thread without blocking the executor
        let result = tokio::task::spawn_blocking(move || {
            let handle = std::thread::spawn(move || {
                run_cpal(stream_clone, tp_clone, format, encoding, vol_clone)
            });
            handle.join()
        })
        .await;

        match result {
            Ok(Ok(Ok(_))) => {
                tracing::info!("Audio format change detected, restarting player");
            }
            Ok(Ok(Err(e))) => {
                tracing::error!(error = %e, "Audio output failed, retrying in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Ok(Err(_)) => {
                tracing::error!("Audio thread panicked, restarting in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "Task join failed, restarting in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

fn run_cpal(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    format: snapcast_proto::SampleFormat,
    encoding: SampleEncoding,
    volume: Arc<VolumeState>,
) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;

    tracing::info!(device = %device.name().unwrap_or_default(), "Using audio device");

    // Try to match stream format; if unsupported, either resample (feature-gated)
    // or fail fast with an explicit error.
    let supported_formats = device.supported_output_configs()?;
    let mut target_config = None;
    for f in supported_formats {
        if f.channels() == format.channels()
            && f.min_sample_rate().0 <= format.rate()
            && f.max_sample_rate().0 >= format.rate()
        {
            target_config = Some(f.with_sample_rate(cpal::SampleRate(format.rate())));
            break;
        }
    }

    #[cfg(feature = "resampler")]
    let (config, _use_resampler): (cpal::StreamConfig, bool) = if let Some(c) = target_config {
        (c.into(), false)
    } else {
        tracing::warn!("Stream format not supported by device, using default and resampling");
        let c = device.default_output_config()?;
        (c.into(), true)
    };

    #[cfg(not(feature = "resampler"))]
    let config: cpal::StreamConfig = if let Some(c) = target_config {
        c.into()
    } else {
        anyhow::bail!(
            "stream format {} Hz/{} ch unsupported by output device and resampler feature is disabled",
            format.rate(),
            format.channels()
        );
    };

    let device_rate = config.sample_rate.0;
    let device_channels = config.channels as usize;

    #[cfg(feature = "resampler")]
    let mut resampler = if _use_resampler {
        let device_format =
            snapcast_proto::SampleFormat::new(device_rate, 16, device_channels as u16);
        // We assume 20ms chunks for resampler init (typical for Snapcast)
        snapcast_client::resampler::Resampler::new_if_needed(
            format,
            device_format,
            encoding,
            (format.rate() / 50) as usize,
        )?
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
        &config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let num_frames = data.len() / device_channels;

            let buffer_dac_usec = info
                .timestamp()
                .playback
                .duration_since(&info.timestamp().callback)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0)
                + (num_frames as i64 * 1_000_000) / device_rate as i64;

            let server_now = {
                let tp = tp_cb.lock().unwrap_or_else(|e| e.into_inner());
                now_usec() + tp.diff_to_server_usec()
            };

            let mut s = stream_cb.lock().unwrap_or_else(|e| e.into_inner());
            let current_format = s.format();
            let current_encoding = s.encoding();

            // Format change detection
            if current_format.rate() != format.rate()
                || current_format.channels() != format.channels()
                || current_encoding != encoding
            {
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
                // Resampling: we need to calculate how many input frames we need to get num_frames output
                // Rubato FftFixedIn is easier if we just process what we get.
                // For simplicity, we read a block from Stream, resample it, and buffer the rest.
                // But cpal callback must be fast.
                // A better approach is to have Stream return what it has and resample that.

                // For now, let's do a simple implementation that matches the requested output frames
                let input_frames =
                    (num_frames as f64 * format.rate() as f64 / device_rate as f64).ceil() as usize;
                pcm_buf.resize(input_frames * frame_size, 0);
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
                    &mut pcm_buf,
                    input_frames as u32,
                );
                drop(s);

                if let Err(e) = r.process(&mut pcm_buf) {
                    tracing::error!(error = %e, "Resampling failed");
                    data.fill(0.0);
                    return;
                }

                write_samples_to_output(
                    data,
                    &pcm_buf,
                    snapcast_proto::SampleFormat::new(device_rate, 32, device_channels as u16),
                    SampleEncoding::Float32,
                );
            } else {
                pcm_buf.resize(num_frames * frame_size, 0);
                s.get_player_chunk_or_silence(
                    server_now,
                    buffer_dac_usec,
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
                    buffer_dac_usec,
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

    cpal_stream.play()?;
    tracing::info!("Audio playback started");

    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let s = stream.lock().unwrap_or_else(|e| e.into_inner());
        let current_format = s.format();
        if current_format.rate() != format.rate()
            || current_format.channels() != format.channels()
            || s.encoding() != encoding
        {
            return Ok(());
        }
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
}
