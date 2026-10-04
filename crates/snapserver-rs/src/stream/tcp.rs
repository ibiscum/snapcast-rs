//! TCP stream reader — accepts a TCP connection and reads PCM from it.

use anyhow::{Result, ensure};
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start a TCP listener that reads PCM from connecting clients.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let (host, port) = bind_target(&uri);
    let chunk_bytes = compute_chunk_bytes(format, chunk_frames)?;

    Ok(tokio::spawn(async move {
        let listener = match TcpListener::bind((host.as_str(), port)).await {
            Ok(l) => {
                tracing::info!(bind_address = %host, port, "TCP stream listening");
                l
            }
            Err(e) => {
                tracing::error!(bind_address = %host, port, error = %e, "Failed to bind TCP stream");
                return;
            }
        };

        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            match listener.accept().await {
                Ok((mut stream, peer)) => {
                    tracing::info!(%peer, "TCP stream client connected");
                    match pump_pcm(&mut stream, &mut ts, chunk_frames, chunk_bytes, &tx, None).await
                    {
                        PumpEnd::SourceEnded => {
                            tracing::info!(%peer, "TCP stream client disconnected");
                            ts.reset();
                        }
                        PumpEnd::TxClosed => return,
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "TCP accept failed");
                }
            }
        }
    }))
}

fn bind_target(uri: &StreamUri) -> (String, u16) {
    let host = if uri.host.is_empty() {
        snapcast_proto::DEFAULT_BIND_ADDRESS.to_string()
    } else {
        uri.host.clone()
    };
    let port = if uri.port == 0 { 4953 } else { uri.port };
    (host, port)
}

fn compute_chunk_bytes(format: SampleFormat, chunk_frames: usize) -> Result<usize> {
    let frame_size = format.frame_size() as usize;
    ensure!(format.rate() > 0, "tcp stream requires sample rate > 0");
    ensure!(frame_size > 0, "tcp stream requires non-zero frame size");
    ensure!(chunk_frames > 0, "tcp stream requires chunk_frames > 0");

    let chunk_bytes = chunk_frames.saturating_mul(frame_size);
    ensure!(chunk_bytes > 0, "tcp stream chunk size must be > 0 bytes");
    Ok(chunk_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn bind_target_uses_defaults_when_host_or_port_missing() {
        let uri = StreamUri {
            scheme: "tcp".into(),
            host: String::new(),
            port: 0,
            path: String::new(),
            query: HashMap::new(),
        };
        let (host, port) = bind_target(&uri);
        assert_eq!(host, snapcast_proto::DEFAULT_BIND_ADDRESS);
        assert_eq!(port, 4953);
    }

    #[test]
    fn bind_target_preserves_explicit_values() {
        let uri = StreamUri {
            scheme: "tcp".into(),
            host: "127.0.0.1".into(),
            port: 6000,
            path: String::new(),
            query: HashMap::new(),
        };
        let (host, port) = bind_target(&uri);
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 6000);
    }

    #[test]
    fn compute_chunk_bytes_rejects_invalid_inputs() {
        let err = compute_chunk_bytes(SampleFormat::new(0, 16, 2), 960).unwrap_err();
        assert!(err.to_string().contains("sample rate > 0"));
        let err = compute_chunk_bytes(SampleFormat::new(48_000, 16, 2), 0).unwrap_err();
        assert!(err.to_string().contains("chunk_frames > 0"));
    }

    #[test]
    fn compute_chunk_bytes_returns_expected_size() {
        let bytes = compute_chunk_bytes(SampleFormat::new(48_000, 16, 2), 960).unwrap();
        assert_eq!(bytes, 960 * 4);
    }
}
