//! Logging setup — wires `--logsink` and `--logfilter` into tracing.
//!
//! Sink options: `stdout`, `stderr`, `null`, `system` (alias for `stderr`), `file:<path>`
//! Filter format: `<tag>:<level>[,<tag>:<level>]*`
//!   - tag: `*` (all) or a module name like `Stream`, `Controller`
//!   - level: trace, debug, info, notice, warning, error, fatal

use anyhow::{Context, Result, bail};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

/// Convert snapcast-style log filter to tracing EnvFilter syntax.
///
/// Snapcast: `*:info,Stream:debug` → tracing: `info,snapcast_client::stream=debug`
fn convert_filter(filter: &str) -> Result<String> {
    filter
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (tag, level) = part.rsplit_once(':').unwrap_or((part, "info"));
            let tag = tag.trim();
            let level = level.trim().to_lowercase();

            let level = match level.as_str() {
                "fatal" => "error",
                "warning" => "warn",
                "notice" => "info",
                other => other,
            };
            level
                .parse::<LevelFilter>()
                .with_context(|| format!("invalid log level for tag '{tag}': {level}"))?;

            if tag == "*" {
                Ok(level.to_string())
            } else {
                let tag_lower = tag.to_lowercase();
                let module = match tag_lower.as_str() {
                    "stream" => "snapcast_client::stream",
                    "controller" => "snapcast_client::controller",
                    "connection" => "snapcast_client::connection",
                    "timeprovider" | "time_provider" => "snapcast_client::time_provider",
                    "player" | "coreaudioplayer" | "alsaplayer" | "pulseplayer" => {
                        "snapclient_rs::player"
                    }
                    "flac" | "flacdecoder" => "snapcast_client::decoder::flac",
                    "opus" | "opusdecoder" => "snapcast_client::decoder::opus",
                    "ogg" | "oggdecoder" => "snapcast_client::decoder::vorbis",
                    "stats" | "latency" => "snapcast_client::stream",
                    _ if tag.contains("::") => tag,
                    other => other,
                };
                Ok(format!("{module}={level}"))
            }
        })
        .collect::<Result<Vec<_>>>()
        .map(|directives| directives.join(","))
}

/// Initialize logging from CLI options.
///
/// A nonblank `RUST_LOG` overrides the provided filter. Invalid filters are errors.
pub(crate) fn init(sink: &str, filter: &str) -> Result<()> {
    let env_filter = match std::env::var("RUST_LOG") {
        Ok(value) if !value.trim().is_empty() => {
            EnvFilter::try_new(value.trim()).context("invalid RUST_LOG filter")?
        }
        Ok(_) | Err(std::env::VarError::NotPresent) => {
            EnvFilter::try_new(convert_filter(filter)?).context("invalid --logfilter")?
        }
        Err(error) => return Err(error).context("could not read RUST_LOG"),
    };

    match sink {
        "null" => {
            // No output
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(std::io::sink)
                .init();
        }
        "stdout" | "" => {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(std::io::stdout)
                .init();
        }
        "stderr" | "system" => {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(std::io::stderr)
                .init();
        }
        s if s.starts_with("file:") => {
            let path = &s[5..];
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| anyhow::anyhow!("failed to open log file {path}: {e}"))?;
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(file)
                .with_ansi(false)
                .init();
        }
        other => {
            bail!("invalid log sink: {other} (expected stdout|stderr|null|system|file:<path>)")
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_default_filter() {
        assert_eq!(convert_filter("*:info").unwrap(), "info");
    }

    #[test]
    fn convert_multi_filter() {
        let result = convert_filter("*:info,Stream:debug").unwrap();
        assert_eq!(result, "info,snapcast_client::stream=debug");
    }

    #[test]
    fn convert_warning_to_warn() {
        assert_eq!(convert_filter("*:warning").unwrap(), "warn");
    }

    #[test]
    fn convert_fatal_to_error() {
        assert_eq!(convert_filter("*:fatal").unwrap(), "error");
    }

    #[test]
    fn convert_notice_to_info() {
        assert_eq!(convert_filter("*:notice").unwrap(), "info");
    }

    #[test]
    fn convert_controller_tag() {
        let result = convert_filter("Controller:trace").unwrap();
        assert_eq!(result, "snapcast_client::controller=trace");
    }

    #[test]
    fn convert_filter_trims_whitespace() {
        let result = convert_filter(" *:warning, Stream:debug ").unwrap();
        assert_eq!(result, "warn,snapcast_client::stream=debug");
    }

    #[test]
    fn convert_player_tag_targets_snapclient_player_module() {
        let result = convert_filter("Player:debug").unwrap();
        assert_eq!(result, "snapclient_rs::player=debug");
    }

    #[test]
    fn convert_unknown_tag_passthrough() {
        let result = convert_filter("my_mod:INFO").unwrap();
        assert_eq!(result, "my_mod=info");
    }

    #[test]
    fn logging_probe() -> Result<()> {
        if let Ok(sink) = std::env::var("SNAPCLIENT_TEST_LOG_SINK") {
            init(&sink, &std::env::var("SNAPCLIENT_TEST_LOG_FILTER")?)?;
            tracing::info!("probe-info-event");
            tracing::debug!(target: "snapcast_client::stream", "probe-stream-debug-event");
            tracing::debug!(target: "snapcast_client::controller", "probe-controller-debug-event");
        }
        Ok(())
    }

    fn run_probe(sink: &str, filter: &str, rust_log: Option<&str>) -> std::process::Output {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "logging::tests::logging_probe", "--nocapture"])
            .env("SNAPCLIENT_TEST_LOG_SINK", sink)
            .env("SNAPCLIENT_TEST_LOG_FILTER", filter);
        match rust_log {
            Some(value) => command.env("RUST_LOG", value),
            None => command.env_remove("RUST_LOG"),
        };
        command.output().unwrap()
    }

    #[test]
    fn log_filters_select_events_and_reject_malformed_levels() {
        let filter = "*:info,snapcast_client::stream:debug";
        for rust_log in [None, Some(""), Some("  "), Some("warn")] {
            let output = run_probe("stdout", filter, rust_log);
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let cli_filter_applies = rust_log != Some("warn");
            assert_eq!(stdout.contains("probe-info-event"), cli_filter_applies);
            assert_eq!(
                stdout.contains("probe-stream-debug-event"),
                cli_filter_applies
            );
            assert!(!stdout.contains("probe-controller-debug-event"));
        }
        for (filter, rust_log, diagnostic) in [
            ("*:bogus", None, "invalid log level for tag '*'"),
            ("Stream:bogus", None, "invalid log level for tag 'Stream'"),
            ("*:info", Some("stream=bogus"), "invalid RUST_LOG filter"),
        ] {
            let output = run_probe("stdout", filter, rust_log);
            assert!(!output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        }
    }

    #[test]
    fn log_sinks_route_or_discard_events() {
        for (sink, writes_stderr) in [("stderr", true), ("system", true), ("null", false)] {
            let output = run_probe(sink, "*:info", None);
            assert!(output.status.success(), "{output:?}");
            assert!(!String::from_utf8_lossy(&output.stdout).contains("probe-info-event"));
            assert_eq!(
                String::from_utf8_lossy(&output.stderr).contains("probe-info-event"),
                writes_stderr
            );
        }
    }

    #[test]
    fn file_sink_appends_plain_text_events() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("snapclient-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("client.log");
        let sink = format!("file:{}", path.display());
        let first = run_probe(&sink, "*:info", None);
        let second = run_probe(&sink, "*:info", None);
        let contents = std::fs::read_to_string(&path);
        let remove_file = std::fs::remove_file(&path);
        let remove_dir = std::fs::remove_dir(&dir);
        assert!(first.status.success(), "{first:?}");
        assert!(second.status.success(), "{second:?}");
        remove_file.unwrap();
        remove_dir.unwrap();
        let contents = contents.unwrap();
        assert_eq!(contents.matches("probe-info-event").count(), 2);
        assert!(!contents.contains('\x1b'));
    }
}
