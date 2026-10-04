//! Librespot stream reader — spawns librespot, parses metadata from stderr.

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

/// Start librespot and read PCM from stdout, metadata from stderr.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    tx: mpsc::Sender<AudioFrame>,
    meta_tx: mpsc::Sender<(String, String)>,
) -> Result<JoinHandle<()>> {
    let devicename = uri.param("devicename").unwrap_or("Snapcast").to_string();
    let bitrate = uri.param("bitrate").unwrap_or("320").to_string();
    let username = uri.param("username").unwrap_or("").to_string();
    let password = uri.param("password").unwrap_or("").to_string();
    let cache = uri.param("cache").unwrap_or("").to_string();
    let volume = uri.param("volume").unwrap_or("100").to_string();
    let normalize = uri.param("normalize").unwrap_or("false") == "true";
    let autoplay = uri.param("autoplay").unwrap_or("false") == "true";
    let (args, log_args) = build_librespot_args(
        &devicename,
        &bitrate,
        &username,
        &password,
        &cache,
        &volume,
        normalize,
        autoplay,
    );

    let frame_size = format.frame_size() as usize;
    ensure!(format.rate() > 0, "librespot stream requires sample rate > 0");
    ensure!(frame_size > 0, "librespot stream requires non-zero frame size");
    let chunk_bytes = (format.rate() as usize * frame_size * 20) / 1000;
    ensure!(chunk_bytes > 0, "librespot stream chunk size must be > 0");
    let chunk_frames = chunk_bytes / frame_size;
    ensure!(chunk_frames > 0, "librespot stream chunk frames must be > 0");

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            tracing::info!(args = ?log_args, "Starting librespot");
            let Ok(mut child) = Command::new("librespot")
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            else {
                tracing::error!("Failed to start librespot");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            };

            let Some(stdout) = child.stdout.take() else {
                let _ = child.kill().await;
                continue;
            };
            let stderr = child.stderr.take();

            // Spawn stderr metadata parser
            let meta_tx2 = meta_tx.clone();
            if let Some(stderr) = stderr {
                tokio::spawn(async move {
                    parse_librespot_stderr(stderr, meta_tx2).await;
                });
            }

            // Read PCM from stdout
            let mut reader = BufReader::new(stdout);
            if let PumpEnd::TxClosed =
                pump_pcm(&mut reader, &mut ts, chunk_frames, chunk_bytes, &tx, None).await
            {
                let _ = child.kill().await;
                return;
            }

            let _ = child.kill().await;
            tracing::info!("librespot exited, restarting");
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }))
}

fn build_librespot_args(
    devicename: &str,
    bitrate: &str,
    username: &str,
    password: &str,
    cache: &str,
    volume: &str,
    normalize: bool,
    autoplay: bool,
) -> (Vec<String>, Vec<String>) {
    let mut args = vec![
        "--name".into(),
        devicename.into(),
        "--bitrate".into(),
        bitrate.into(),
        "--backend".into(),
        "pipe".into(),
        "--initial-volume".into(),
        volume.into(),
        "--verbose".into(),
    ];
    let mut log_args = args.clone();

    if !username.is_empty() && !password.is_empty() {
        args.extend([
            "--username".into(),
            username.into(),
            "--password".into(),
            password.into(),
        ]);
        log_args.extend([
            "--username".into(),
            username.into(),
            "--password".into(),
            "******".into(),
        ]);
    } else if !username.is_empty() || !password.is_empty() {
        tracing::warn!("Ignoring partial librespot credentials: both username and password are required");
    }

    if !cache.is_empty() {
        args.extend(["--cache".into(), cache.into()]);
        log_args.extend(["--cache".into(), cache.into()]);
    }
    if normalize {
        args.push("--enable-volume-normalisation".into());
        log_args.push("--enable-volume-normalisation".into());
    }
    if autoplay {
        args.extend(["--autoplay".into(), "on".into()]);
        log_args.extend(["--autoplay".into(), "on".into()]);
    }
    (args, log_args)
}

fn parse_librespot_loaded_title(line: &str) -> Option<String> {
    let start = line.find('<')?;
    let end = line[start + 1..].find('>')? + start + 1;
    if !line.contains("ms) loaded") || start >= end {
        return None;
    }
    let title = &line[start + 1..end];
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// Parse librespot stderr for track metadata.
/// Format: `[...INFO  librespot_playback::player] <Track Name> (123456 ms) loaded`
async fn parse_librespot_stderr<R: AsyncRead + Unpin>(
    stderr: R,
    meta_tx: mpsc::Sender<(String, String)>,
) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(title) = parse_librespot_loaded_title(&line) {
            tracing::info!(title, "Librespot: track loaded");
            let _ = meta_tx.send(("title".into(), title)).await;
        }
        // Forward log lines at appropriate level
        if line.contains("ERROR") {
            tracing::error!(target: "librespot", "{line}");
        } else if line.contains("WARN") {
            tracing::warn!(target: "librespot", "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_librespot_args_masks_password_in_logs() {
        let (args, log_args) = build_librespot_args(
            "Snapcast",
            "320",
            "alice",
            "secret",
            "",
            "100",
            false,
            false,
        );
        assert!(args.windows(2).any(|w| w == ["--password", "secret"]));
        assert!(log_args.windows(2).any(|w| w == ["--password", "******"]));
    }

    #[test]
    fn build_librespot_args_ignores_partial_credentials() {
        let (args, log_args) = build_librespot_args(
            "Snapcast",
            "320",
            "alice",
            "",
            "",
            "100",
            false,
            false,
        );
        assert!(!args.iter().any(|a| a == "--username" || a == "--password"));
        assert!(!log_args.iter().any(|a| a == "--username" || a == "--password"));
    }

    #[test]
    fn parse_librespot_loaded_title_extracts_title() {
        let line = "[INFO librespot_playback::player] <Song Name> (123456 ms) loaded";
        assert_eq!(
            parse_librespot_loaded_title(line).as_deref(),
            Some("Song Name")
        );
    }

    #[test]
    fn parse_librespot_loaded_title_rejects_non_matching_lines() {
        assert!(parse_librespot_loaded_title("no metadata").is_none());
        assert!(parse_librespot_loaded_title("<x> loaded").is_none());
        assert!(parse_librespot_loaded_title("<> (100 ms) loaded").is_none());
    }
}
