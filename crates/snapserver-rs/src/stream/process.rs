//! Process stream reader — captures stdout PCM from a child process.

use anyhow::{Result, ensure};
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start a child process and read PCM from its stdout.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let params = uri.param("params").unwrap_or("").to_string();
    let args = parse_process_params(&params)?;
    let chunk_bytes = compute_chunk_bytes(format, chunk_frames)?;

    Ok(tokio::spawn(async move {
        loop {
            tracing::info!(path, args = ?args, "Starting process stream");

            let Ok(mut child) = Command::new(&path)
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            else {
                tracing::error!(path, "Failed to start process");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            };

            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill().await;
                continue;
            };

            if let Some(stderr) = child.stderr.take() {
                tokio::spawn(async move {
                    log_process_stderr(stderr).await;
                });
            }

            // Anchor timestamps at process-start time for each run.
            let mut ts = ChunkTimestamper::new(format.rate());
            if let PumpEnd::TxClosed =
                pump_pcm(&mut stdout, &mut ts, chunk_frames, chunk_bytes, &tx, None).await
            {
                let _ = child.kill().await;
                return;
            }

            let _ = child.kill().await;
            tracing::info!(path, "Process exited, restarting");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}

fn compute_chunk_bytes(format: SampleFormat, chunk_frames: usize) -> Result<usize> {
    let frame_size = format.frame_size() as usize;
    ensure!(format.rate() > 0, "process stream requires sample rate > 0");
    ensure!(frame_size > 0, "process stream requires non-zero frame size");
    ensure!(chunk_frames > 0, "process stream requires chunk_frames > 0");

    let chunk_bytes = chunk_frames.saturating_mul(frame_size);
    ensure!(chunk_bytes > 0, "process stream chunk size must be > 0 bytes");
    Ok(chunk_bytes)
}

fn parse_process_params(input: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                let Some(next) = chars.next() else {
                    anyhow::bail!("invalid params: trailing escape");
                };
                cur.push(next);
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }

    ensure!(
        !in_single && !in_double,
        "invalid params: unterminated quote"
    );
    if !cur.is_empty() {
        args.push(cur);
    }
    Ok(args)
}

async fn log_process_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.contains("ERROR") {
            tracing::error!(target: "process-stream", "{line}");
        } else if line.contains("WARN") {
            tracing::warn!(target: "process-stream", "{line}");
        } else {
            tracing::debug!(target: "process-stream", "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn parse_process_params_supports_quotes_and_escapes() {
        let args = parse_process_params("foo --opt \"a b\" 'c d' e\\ f").unwrap();
        assert_eq!(args, vec!["foo", "--opt", "a b", "c d", "e f"]);
    }

    #[test]
    fn parse_process_params_rejects_invalid_input() {
        let err = parse_process_params("'unterminated").unwrap_err();
        assert!(err.to_string().contains("unterminated quote"));
        let err = parse_process_params("abc\\").unwrap_err();
        assert!(err.to_string().contains("trailing escape"));
    }
}
