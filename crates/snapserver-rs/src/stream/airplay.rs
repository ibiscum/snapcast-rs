//! Airplay stream reader — spawns shairport-sync, parses metadata from pipe.

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

/// Start shairport-sync and read PCM from stdout.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    tx: mpsc::Sender<AudioFrame>,
    meta_tx: mpsc::Sender<(String, String)>,
) -> Result<JoinHandle<()>> {
    let devicename = uri.param("devicename").unwrap_or("Snapcast").to_string();
    let port = uri.param("port").unwrap_or("5000").to_string();
    let password = uri.param("password").unwrap_or("").to_string();
    let (args, log_args) = build_shairport_args(&devicename, &port, &password)?;

    let frame_size = format.frame_size() as usize;
    ensure!(format.rate() > 0, "airplay stream requires sample rate > 0");
    ensure!(frame_size > 0, "airplay stream requires non-zero frame size");
    let chunk_bytes = (format.rate() as usize * frame_size * 20) / 1000;
    ensure!(chunk_bytes > 0, "airplay stream chunk size must be > 0");
    let chunk_frames = chunk_bytes / frame_size;
    ensure!(chunk_frames > 0, "airplay stream chunk frames must be > 0");

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            tracing::info!(args = ?log_args, "Starting shairport-sync");
            let Ok(mut child) = Command::new("shairport-sync")
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            else {
                tracing::error!("Failed to start shairport-sync");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            };

            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill().await;
                continue;
            };

            // Parse stderr for metadata (shairport-sync logs track info)
            if let Some(stderr) = child.stderr.take() {
                let meta_tx2 = meta_tx.clone();
                tokio::spawn(async move {
                    parse_airplay_stderr(stderr, meta_tx2).await;
                });
            }

            if let PumpEnd::TxClosed =
                pump_pcm(&mut stdout, &mut ts, chunk_frames, chunk_bytes, &tx, None).await
            {
                let _ = child.kill().await;
                return;
            }

            let _ = child.kill().await;
            tracing::info!("shairport-sync exited, restarting");
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }))
}

fn build_shairport_args(
    devicename: &str,
    port: &str,
    password: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let port: u16 = port
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid airplay port '{port}': {e}"))?;

    let mut args = vec![
        "--name".to_string(),
        devicename.to_string(),
        "--output=stdout".to_string(),
        "--get-coverart".to_string(),
        "--port".to_string(),
        port.to_string(),
    ];
    let mut log_args = args.clone();
    if !password.is_empty() {
        args.extend(["--password".to_string(), password.to_string()]);
        log_args.extend(["--password".to_string(), "******".to_string()]);
    }
    Ok((args, log_args))
}

fn parse_airplay_metadata_line(line: &str) -> Option<(&'static str, String)> {
    for (prefix, key) in [
        ("Title: ", "title"),
        ("Artist: ", "artist"),
        ("Album: ", "album"),
    ] {
        if let Some((_, value)) = line.split_once(prefix) {
            let value = value.trim();
            if !value.is_empty() {
                return Some((key, value.to_string()));
            }
        }
    }
    None
}

/// Parse shairport-sync stderr for metadata.
async fn parse_airplay_stderr<R: AsyncRead + Unpin>(
    stderr: R,
    meta_tx: mpsc::Sender<(String, String)>,
) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // shairport-sync metadata format varies by version
        // Common: "Title: ...", "Artist: ...", "Album: ..."
        if let Some((key, value)) = parse_airplay_metadata_line(&line) {
            if key == "title" {
                tracing::info!(title = %value, "Airplay: track");
            }
            let _ = meta_tx.send((key.to_string(), value)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_shairport_args_masks_password_in_logs() {
        let (args, log_args) = build_shairport_args("Snapcast", "5000", "secret").unwrap();
        assert_eq!(
            args,
            vec![
                "--name".to_string(),
                "Snapcast".to_string(),
                "--output=stdout".to_string(),
                "--get-coverart".to_string(),
                "--port".to_string(),
                "5000".to_string(),
                "--password".to_string(),
                "secret".to_string(),
            ]
        );
        assert_eq!(
            log_args,
            vec![
                "--name".to_string(),
                "Snapcast".to_string(),
                "--output=stdout".to_string(),
                "--get-coverart".to_string(),
                "--port".to_string(),
                "5000".to_string(),
                "--password".to_string(),
                "******".to_string(),
            ]
        );
    }

    #[test]
    fn build_shairport_args_rejects_invalid_port() {
        let err = build_shairport_args("Snapcast", "not-a-port", "").unwrap_err();
        assert!(err.to_string().contains("invalid airplay port"));
    }

    #[test]
    fn parse_airplay_metadata_line_accepts_prefixed_logs() {
        let parsed = parse_airplay_metadata_line("[info] Artist: Massive Attack").unwrap();
        assert_eq!(parsed.0, "artist");
        assert_eq!(parsed.1, "Massive Attack");
    }

    #[test]
    fn parse_airplay_metadata_line_ignores_unknown_or_empty_values() {
        assert!(parse_airplay_metadata_line("something else").is_none());
        assert!(parse_airplay_metadata_line("Title: ").is_none());
    }
}
