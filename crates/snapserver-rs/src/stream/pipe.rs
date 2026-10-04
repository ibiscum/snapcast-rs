//! Pipe stream reader — reads PCM from a named pipe (FIFO).

use anyhow::{Result, ensure};
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start reading PCM from a named pipe.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let chunk_bytes = compute_chunk_bytes(format, chunk_frames)?;

    Ok(tokio::spawn(async move {
        loop {
            match tokio::fs::OpenOptions::new().read(true).open(&path).await {
                Ok(mut file) => {
                    tracing::info!(path, "Pipe stream opened");
                    let mut ts = ChunkTimestamper::new(format.rate());
                    match pump_pcm(&mut file, &mut ts, chunk_frames, chunk_bytes, &tx, None).await {
                        PumpEnd::SourceEnded => {
                            tracing::debug!(path, "Pipe read ended, reopening");
                        }
                        PumpEnd::TxClosed => return,
                    }
                }
                Err(e) => {
                    tracing::warn!(path, error = %e, "Pipe not available, retrying");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}

fn compute_chunk_bytes(format: SampleFormat, chunk_frames: usize) -> Result<usize> {
    let frame_size = format.frame_size() as usize;
    ensure!(format.rate() > 0, "pipe stream requires sample rate > 0");
    ensure!(frame_size > 0, "pipe stream requires non-zero frame size");
    ensure!(chunk_frames > 0, "pipe stream requires chunk_frames > 0");

    let chunk_bytes = chunk_frames.saturating_mul(frame_size);
    ensure!(chunk_bytes > 0, "pipe stream chunk size must be > 0 bytes");
    Ok(chunk_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_chunk_bytes_rejects_zero_rate() {
        let err = compute_chunk_bytes(SampleFormat::new(0, 16, 2), 960).unwrap_err();
        assert!(err.to_string().contains("sample rate > 0"));
    }

    #[test]
    fn compute_chunk_bytes_rejects_zero_chunk_frames() {
        let err = compute_chunk_bytes(SampleFormat::new(48_000, 16, 2), 0).unwrap_err();
        assert!(err.to_string().contains("chunk_frames > 0"));
    }

    #[test]
    fn compute_chunk_bytes_returns_expected_size() {
        let bytes = compute_chunk_bytes(SampleFormat::new(48_000, 16, 2), 960).unwrap();
        assert_eq!(bytes, 960 * 4);
    }
}
