//! File stream reader — reads PCM/WAV from a file, loops on EOF.

use anyhow::{Result, ensure};
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start reading PCM from a file, looping on EOF.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let (chunk_bytes, chunk_duration) = compute_chunk_params(format, chunk_frames)?;

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            match tokio::fs::File::open(&path).await {
                Ok(mut file) => {
                    tracing::info!(path, "File stream opened");
                    if let Err(e) = seek_past_wav_header_if_present(&mut file).await {
                        tracing::warn!(path, error = %e, "Failed to parse WAV header, retrying");
                    } else {
                        // Paced reads so a finite file plays back in real time.
                        match pump_pcm(
                            &mut file,
                            &mut ts,
                            chunk_frames,
                            chunk_bytes,
                            &tx,
                            Some(chunk_duration),
                        )
                        .await
                        {
                            PumpEnd::SourceEnded => {} // EOF → reopen and loop
                            PumpEnd::TxClosed => return,
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(path, error = %e, "Failed to open file stream, retrying");
                }
            }
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}

fn compute_chunk_params(format: SampleFormat, chunk_frames: usize) -> Result<(usize, std::time::Duration)> {
    let frame_size = format.frame_size() as usize;
    ensure!(format.rate() > 0, "file stream requires sample rate > 0");
    ensure!(frame_size > 0, "file stream requires non-zero frame size");
    ensure!(chunk_frames > 0, "file stream requires chunk_frames > 0");

    let chunk_bytes = chunk_frames.saturating_mul(frame_size);
    ensure!(chunk_bytes > 0, "file stream chunk size must be > 0 bytes");

    let chunk_micros = (chunk_frames as u64).saturating_mul(1_000_000) / format.rate() as u64;
    ensure!(
        chunk_micros > 0,
        "file stream chunk duration must be > 0µs for pacing"
    );
    Ok((chunk_bytes, std::time::Duration::from_micros(chunk_micros)))
}

/// If the file is RIFF/WAVE, seek to the start of the `data` chunk payload.
/// Otherwise, reset to start and treat it as raw PCM.
async fn seek_past_wav_header_if_present(file: &mut tokio::fs::File) -> Result<()> {
    use std::io::SeekFrom;

    let mut riff_header = [0u8; 12];
    if file.read_exact(&mut riff_header).await.is_err() {
        file.seek(SeekFrom::Start(0)).await?;
        return Ok(());
    }

    if &riff_header[..4] != b"RIFF" || &riff_header[8..12] != b"WAVE" {
        file.seek(SeekFrom::Start(0)).await?;
        return Ok(());
    }

    loop {
        let mut chunk_header = [0u8; 8];
        if file.read_exact(&mut chunk_header).await.is_err() {
            anyhow::bail!("WAV file missing data chunk");
        }
        let chunk_id = &chunk_header[..4];
        let chunk_size = u32::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ]) as u64;

        if chunk_id == b"data" {
            return Ok(());
        }

        let pad = chunk_size % 2;
        let skip = chunk_size + pad;
        file.seek(SeekFrom::Current(skip as i64)).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_chunk_params_rejects_invalid_inputs() {
        let err = compute_chunk_params(SampleFormat::new(0, 16, 2), 960).unwrap_err();
        assert!(err.to_string().contains("sample rate > 0"));

        let err = compute_chunk_params(SampleFormat::new(48_000, 16, 2), 0).unwrap_err();
        assert!(err.to_string().contains("chunk_frames > 0"));
    }

    #[test]
    fn compute_chunk_params_returns_expected_values() {
        let (bytes, dur) = compute_chunk_params(SampleFormat::new(48_000, 16, 2), 960).unwrap();
        assert_eq!(bytes, 960 * 4);
        assert_eq!(dur, std::time::Duration::from_millis(20));
    }

    #[tokio::test]
    async fn seek_wav_data_chunk_skips_to_payload() {
        let mut wav = Vec::new();
        // RIFF header: RIFF + size + WAVE
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        // fmt chunk (8 + 16)
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&(16u32).to_le_bytes());
        wav.extend_from_slice(&[0u8; 16]);
        // data chunk header + payload
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(4u32).to_le_bytes());
        wav.extend_from_slice(&[1u8, 2, 3, 4]);

        let path = std::env::temp_dir().join(format!(
            "snapserver-file-test-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, &wav).unwrap();

        let mut file = tokio::fs::File::open(&path).await.unwrap();
        seek_past_wav_header_if_present(&mut file).await.unwrap();
        let mut buf = [0u8; 4];
        file.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn seek_non_wav_resets_to_start() {
        let path = std::env::temp_dir().join(format!(
            "snapserver-file-test-{}-{}.pcm",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"abcd").unwrap();

        let mut file = tokio::fs::File::open(&path).await.unwrap();
        seek_past_wav_header_if_present(&mut file).await.unwrap();
        let mut one = [0u8; 1];
        file.read_exact(&mut one).await.unwrap();
        assert_eq!(one, [b'a']);

        let _ = std::fs::remove_file(path);
    }
}
