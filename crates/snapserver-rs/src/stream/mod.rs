//! Stream readers for the snapserver-rs binary.

use std::time::Duration;

use snapcast_server::time::ChunkTimestamper;
use snapcast_server::{AudioData, AudioFrame};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

pub(crate) mod airplay;
pub(crate) mod file;
pub(crate) mod librespot;
pub(crate) mod pipe;
pub(crate) mod process;
pub(crate) mod tcp;
pub(crate) mod uri;

/// Why [`pump_pcm`] returned.
pub(crate) enum PumpEnd {
    /// The source ended (EOF / disconnect / process exit). The caller may
    /// reopen the source and pump again.
    SourceEnded,
    /// The audio channel was closed (the consumer is gone). The caller should
    /// stop and clean up.
    TxClosed,
}

/// Read fixed-size PCM chunks from `reader`, timestamp them, and forward to `tx`.
///
/// The shared inner loop of every stream reader: each chunk is `chunk_bytes`
/// long (= `chunk_frames` frames) and is timestamped via `ts`. When `pace` is
/// `Some`, reads are rate-limited to that interval — used by the file reader so
/// a finite file plays back in real time; the other sources (pipe, socket,
/// child stdout) block naturally and pass `None`. Note: `tokio::time::interval`
/// yields immediately on first tick, so the first chunk is emitted right away.
pub(crate) async fn pump_pcm<R: AsyncRead + Unpin>(
    reader: &mut R,
    ts: &mut ChunkTimestamper,
    chunk_frames: usize,
    chunk_bytes: usize,
    tx: &mpsc::Sender<AudioFrame>,
    pace: Option<Duration>,
) -> PumpEnd {
    if chunk_bytes == 0 || chunk_frames == 0 {
        tracing::error!(chunk_bytes, chunk_frames, "invalid pump_pcm chunk parameters");
        return PumpEnd::SourceEnded;
    }

    let mut buf = vec![0u8; chunk_bytes];
    let mut interval = pace.map(tokio::time::interval);
    loop {
        if let Some(iv) = interval.as_mut() {
            iv.tick().await;
        }
        if reader.read_exact(&mut buf).await.is_err() {
            return PumpEnd::SourceEnded;
        }
        let frame = AudioFrame {
            timestamp_usec: ts.next(chunk_frames as u32),
            data: AudioData::Pcm(buf.clone()),
        };
        if tx.send(frame).await.is_err() {
            return PumpEnd::TxClosed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pump_pcm_returns_source_ended_on_eof() {
        let mut reader = tokio::io::empty();
        let mut ts = ChunkTimestamper::new(48_000);
        let (tx, _rx) = mpsc::channel(1);
        let end = pump_pcm(&mut reader, &mut ts, 960, 960 * 4, &tx, None).await;
        assert!(matches!(end, PumpEnd::SourceEnded));
    }

    #[tokio::test]
    async fn pump_pcm_returns_tx_closed_when_consumer_gone() {
        let mut reader = tokio::io::repeat(0u8);
        let mut ts = ChunkTimestamper::new(48_000);
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let end = pump_pcm(&mut reader, &mut ts, 960, 960 * 4, &tx, None).await;
        assert!(matches!(end, PumpEnd::TxClosed));
    }

    #[tokio::test]
    async fn pump_pcm_rejects_zero_chunk_parameters() {
        let mut reader = tokio::io::repeat(0u8);
        let mut ts = ChunkTimestamper::new(48_000);
        let (tx, _rx) = mpsc::channel(1);
        let end = pump_pcm(&mut reader, &mut ts, 0, 0, &tx, None).await;
        assert!(matches!(end, PumpEnd::SourceEnded));
    }
}
