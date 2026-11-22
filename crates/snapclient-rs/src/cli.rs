//! CLI argument parsing — maps command-line args to [`ClientSettings`].

use std::{net::Ipv6Addr, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

use snapcast_client::config::{self, Auth, ClientSettings, MixerMode, ServerSettings};

/// Snapcast client — synchronized multiroom audio player.
#[derive(Parser, Debug)]
#[command(
    version,
    about,
    after_help = "\
  With 'url' = tcp://<snapserver hostname or IP address>[:port]\n\
  For example: 'tcp://192.168.1.1:1704', or 'tcp://[::1]:1704'\n\
  If 'url' is not configured, snapclient uses mDNS discovery when enabled,\n\
  otherwise it defaults to 'tcp://localhost:1704'"
)]
pub struct Cli {
    /// Snapserver URL: `tcp://<host>[:<port>]`
    pub url: Option<String>,

    /// Instance id when running multiple instances on the same host
    #[arg(short, long, default_value_t = 1)]
    pub instance: u32,

    /// Unique host id (default: MAC address)
    #[arg(long = "hostID", default_value = "")]
    pub host_id: String,

    /// Client certificate file (unsupported for TCP playback)
    #[arg(long = "cert")]
    pub certificate: Option<PathBuf>,

    /// Client private key file (unsupported for TCP playback)
    #[arg(long = "cert-key")]
    pub certificate_key: Option<PathBuf>,

    /// Client private key password (unsupported for TCP playback)
    #[arg(long = "key-password")]
    pub key_password: Option<String>,

    /// Server CA certificate (unsupported for TCP playback)
    #[arg(long = "server-cert", require_equals = true)]
    pub server_certificate: Option<Option<PathBuf>>,

    /// List PCM devices
    #[arg(short, long)]
    pub list: bool,

    /// PCM output device (only "default" is supported)
    #[arg(short, long, default_value = "default")]
    pub soundcard: String,

    /// Additional latency of the audio device (ms)
    #[arg(long, default_value_t = 0)]
    pub latency: i32,

    /// Requested output format (manual override is unsupported)
    #[arg(long)]
    pub sampleformat: Option<String>,

    /// Audio player backend (only "cpal" without parameters is supported for playback)
    #[arg(long, default_value = "")]
    pub player: String,

    /// Mixer mode: `software|hardware|script|none[:<params>]`
    #[arg(long, default_value = "software")]
    pub mixer: String,

    /// Daemonize; set optional priority (-20 to 19) with --daemon=<priority>
    #[cfg(unix)]
    #[arg(short, long, require_equals = true, allow_negative_numbers = true)]
    pub daemon: Option<Option<i32>>,

    /// Daemon user or group override (unsupported)
    #[cfg(unix)]
    #[arg(long)]
    pub user: Option<String>,

    /// Log sink: null|stdout|stderr|system (alias for stderr)|file:<path>
    #[arg(long, default_value = "stdout")]
    pub logsink: String,

    /// Log filter: <tag>:<level>[,<tag>:<level>]* (overridden by nonblank RUST_LOG)
    #[arg(long, default_value = "*:info")]
    pub logfilter: String,

    /// Pre-shared key for f32lz4e decryption (default: built-in key)
    #[cfg(feature = "encryption")]
    #[arg(long)]
    pub encryption_psk: Option<String>,
}

impl Cli {
    /// Parse CLI args and build a [`ClientSettings`].
    pub fn into_settings(self) -> Result<ClientSettings> {
        for (option, supplied) in [
            ("--cert", self.certificate.is_some()),
            ("--cert-key", self.certificate_key.is_some()),
            ("--key-password", self.key_password.is_some()),
            ("--server-cert", self.server_certificate.is_some()),
        ] {
            anyhow::ensure!(
                !supplied,
                "{option} requires TLS, which is not supported for TCP playback"
            );
        }
        anyhow::ensure!(
            self.soundcard == "default",
            "--soundcard only supports the default output device"
        );
        anyhow::ensure!(
            matches!(self.player.as_str(), "" | "cpal"),
            "--player only supports cpal without parameters for playback"
        );
        anyhow::ensure!(
            self.sampleformat.is_none(),
            "--sampleformat is not supported; the player negotiates the output format"
        );
        #[cfg(unix)]
        anyhow::ensure!(
            self.user.is_none(),
            "--user is not supported; start snapclient-rs as the intended user instead"
        );

        let server = match self.url.as_deref() {
            Some(url) => parse_url(url)?,
            None => default_server_settings(),
        };

        // Mixer
        let (mixer_mode_str, mixer_param) = self
            .mixer
            .split_once(':')
            .map(|(m, p)| (m, p.to_string()))
            .unwrap_or((&self.mixer, String::new()));
        let mixer_mode = match mixer_mode_str {
            "software" => MixerMode::Software,
            "hardware" => MixerMode::Hardware,
            "script" => MixerMode::Script,
            "none" => MixerMode::None,
            other => bail!("unknown mixer mode: {other}"),
        };

        #[cfg(unix)]
        if let Some(Some(priority)) = self.daemon {
            anyhow::ensure!(
                (-20..=19).contains(&priority),
                "invalid daemon priority {priority}; expected range -20..=19"
            );
        }

        Ok(ClientSettings {
            instance: self.instance,
            host_id: self.host_id,
            server,
            player: config::PlayerSettings {
                player_name: self.player,
                latency: self.latency,
                mixer: config::MixerSettings {
                    mode: mixer_mode,
                    parameter: mixer_param,
                },
                ..Default::default()
            },
            logging: config::LoggingSettings {
                sink: self.logsink,
                filter: self.logfilter,
            },
            #[cfg(unix)]
            daemon: self.daemon.map(|priority| config::DaemonSettings {
                priority: priority.or(Some(-3)),
                user: None,
            }),
        })
    }
}

fn default_server_settings() -> ServerSettings {
    let mut settings = ServerSettings {
        scheme: snapcast_proto::SCHEME_TCP.to_string(),
        port: snapcast_proto::DEFAULT_STREAM_PORT,
        ..ServerSettings::default()
    };
    #[cfg(feature = "mdns")]
    {
        settings.host.clear();
    }
    #[cfg(not(feature = "mdns"))]
    {
        settings.host = "localhost".to_string();
    }
    settings
}

/// Parse a snapcast URL into [`ServerSettings`].
fn parse_url(url: &str) -> Result<ServerSettings> {
    let mut settings = ServerSettings::default();

    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("invalid URL, expected <scheme>://<host>[:port]: {url}"))?;

    match scheme {
        snapcast_proto::SCHEME_TCP => settings.scheme = scheme.to_string(),
        snapcast_proto::SCHEME_WS | snapcast_proto::SCHEME_WSS => {
            bail!("websocket audio transport is not supported yet; use tcp://")
        }
        _ => bail!("unsupported scheme: {scheme} (expected tcp)"),
    }

    // Extract optional user:password@
    let rest = if let Some((userinfo, host_part)) = rest.rsplit_once('@') {
        let (user, password) = userinfo
            .split_once(':')
            .context("invalid credentials, expected user:password@host")?;
        settings.auth = Some(Auth {
            scheme: "Basic".into(),
            param: base64_encode_credentials(user, password),
        });
        host_part
    } else {
        rest
    };

    let (host, port) = parse_host_port(rest, snapcast_proto::DEFAULT_STREAM_PORT)?;
    settings.host = host;
    settings.port = port;

    Ok(settings)
}

fn parse_host_port(input: &str, default_port: u16) -> Result<(String, u16)> {
    anyhow::ensure!(!input.is_empty(), "missing host");

    if let Some(stripped) = input.strip_prefix('[') {
        let (host, tail) = stripped
            .split_once(']')
            .context("invalid IPv6 host, missing closing ']'")?;
        let address = if let Some((address, zone)) = host.split_once('%') {
            anyhow::ensure!(
                !zone.is_empty()
                    && !zone.contains(['%', ':', '[', ']', '/', '?', '#'])
                    && !zone.contains(char::is_whitespace),
                "invalid IPv6 scope identifier"
            );
            address
        } else {
            host
        };
        address
            .parse::<Ipv6Addr>()
            .context("invalid bracketed IPv6 host")?;
        let port = if tail.is_empty() {
            default_port
        } else {
            let port_str = tail
                .strip_prefix(':')
                .with_context(|| format!("invalid IPv6 host suffix: {tail}"))?;
            parse_port(port_str)?
        };
        return Ok((host.to_string(), port));
    }

    if input.matches(':').count() == 1 {
        let (host, port_str) = input
            .rsplit_once(':')
            .expect("counted exactly one ':' before splitting");
        anyhow::ensure!(!host.is_empty(), "missing host");
        return Ok((host.to_string(), parse_port(port_str)?));
    }

    // No colons — plain hostname or IPv4
    if !input.contains(':') {
        return Ok((input.to_string(), default_port));
    }

    // Multiple colons without brackets — bare IPv6. Accept only if valid IPv6.
    // If the user intended a port, they must use bracket notation: [::1]:1704
    anyhow::ensure!(
        input.parse::<Ipv6Addr>().is_ok(),
        "ambiguous IPv6 address with port — use bracket notation: tcp://[{input}]:port"
    );
    Ok((input.to_string(), default_port))
}

fn parse_port(port_str: &str) -> Result<u16> {
    anyhow::ensure!(!port_str.is_empty(), "missing port");
    port_str
        .parse()
        .with_context(|| format!("invalid port: {port_str}"))
}

fn base64_encode_credentials(user: &str, password: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tcp_url() {
        let s = parse_url("tcp://192.168.1.1:1704").unwrap();
        assert_eq!(s.scheme, "tcp");
        assert_eq!(s.host, "192.168.1.1");
        assert_eq!(s.port, 1704);
        assert!(s.auth.is_none());
    }

    #[test]
    fn parse_ipv6_url() {
        let s = parse_url("tcp://[::1]:1704").unwrap();
        assert_eq!(s.scheme, "tcp");
        assert_eq!(s.host, "::1");
        assert_eq!(s.port, 1704);
    }

    #[test]
    fn parse_ipv6_url_default_port() {
        let s = parse_url("tcp://::1").unwrap();
        assert_eq!(s.host, "::1");
        assert_eq!(s.port, 1704);
    }

    #[test]
    fn parse_ipv6_ambiguous_with_port_rejected() {
        // "::1:99999" is not a valid IPv6 address, so it's rejected as ambiguous
        let err = parse_url("tcp://::1:99999").unwrap_err();
        assert!(err.to_string().contains("bracket notation"));
    }

    #[test]
    fn parse_url_with_credentials() {
        let s = parse_url("tcp://user:pass@myhost:1704").unwrap();
        assert_eq!(s.host, "myhost");
        let auth = s.auth.unwrap();
        assert_eq!(auth.scheme, "Basic");
        assert_eq!(auth.param, "dXNlcjpwYXNz");
    }

    #[test]
    fn parse_invalid_scheme() {
        assert!(parse_url("http://localhost").is_err());
    }

    #[test]
    fn parse_websocket_scheme_is_unsupported() {
        assert!(parse_url("ws://homeserver.local").is_err());
        assert!(parse_url("wss://secure.host:1788").is_err());
    }

    #[test]
    fn parse_invalid_url() {
        assert!(parse_url("garbage").is_err());
    }

    #[test]
    fn parse_hostname_with_default_port() {
        let s = parse_url("tcp://snapserver.local").unwrap();
        assert_eq!(s.scheme, "tcp");
        assert_eq!(s.host, "snapserver.local");
        assert_eq!(s.port, 1704);
    }

    #[test]
    fn omitted_url_uses_feature_dependent_default() {
        let cli = Cli::parse_from(["snapclient-rs"]);
        assert!(cli.url.is_none());
        let settings = cli.into_settings().unwrap();
        #[cfg(feature = "mdns")]
        assert!(settings.server.host.is_empty());
        #[cfg(not(feature = "mdns"))]
        assert_eq!(settings.server.host, "localhost");
    }

    #[test]
    fn parse_url_missing_host_is_invalid() {
        assert!(parse_url("tcp://").is_err());
    }

    #[test]
    fn bracketed_hosts_require_ipv6_and_preserve_scope() {
        for url in [
            "tcp://[not-ipv6]:1704",
            "tcp://[192.0.2.1]:1704",
            "tcp://[]:1704",
            "tcp://[fe80::1%]:1704",
        ] {
            assert!(parse_url(url).is_err(), "{url}");
        }
        let settings = parse_url("tcp://[fe80::1234%wlan0]:1800").unwrap();
        assert_eq!(settings.host, "fe80::1234%wlan0");
        assert_eq!(settings.port, 1800);
    }

    #[test]
    fn cli_into_settings_mixer() {
        let cli = Cli::parse_from(["snapclient-rs", "--mixer", "hardware:hw:0"]);
        let s = cli.into_settings().unwrap();
        assert_eq!(s.player.mixer.mode, MixerMode::Hardware);
        assert_eq!(s.player.mixer.parameter, "hw:0");
    }

    #[test]
    fn cli_into_settings_supported_playback_options() {
        let cli = Cli::parse_from([
            "snapclient-rs",
            "--instance",
            "2",
            "--hostID",
            "my-id",
            "--player",
            "cpal",
            "--latency",
            "50",
            "--soundcard",
            "default",
            "--logsink",
            "stderr",
            "--logfilter",
            "*:debug",
            "tcp://localhost:1704",
        ]);
        let s = cli.into_settings().unwrap();
        assert_eq!(s.instance, 2);
        assert_eq!(s.host_id, "my-id");
        assert_eq!(s.player.player_name, "cpal");
        assert_eq!(s.player.latency, 50);
        assert_eq!(s.player.pcm_device.name, "default");
        assert_eq!(s.logging.sink, "stderr");
        assert_eq!(s.logging.filter, "*:debug");
    }

    #[test]
    fn cli_rejects_unsupported_tls_and_playback_options() {
        for (option, value, diagnostic) in [
            ("--cert", "client.pem", "requires TLS"),
            ("--cert-key", "client.key", "requires TLS"),
            ("--key-password", "example", "requires TLS"),
            ("--soundcard", "hw:1", "only supports the default"),
            ("--player", "alsa", "only supports cpal"),
            ("--player", "cpal:buffer=100", "without parameters"),
            ("--sampleformat", "48000:16:2", "is not supported"),
        ] {
            let err = Cli::try_parse_from(["snapclient-rs", option, value, "tcp://server:1704"])
                .unwrap()
                .into_settings()
                .unwrap_err()
                .to_string();
            assert!(err.contains(option) && err.contains(diagnostic), "{err}");
        }
    }

    #[test]
    fn cli_list_flag() {
        let cli = Cli::parse_from(["snapclient-rs", "--list"]);
        assert!(cli.list);
    }

    #[test]
    fn cli_server_cert_never_consumes_the_url_and_is_rejected_for_tcp() {
        let url = "tcp://server:1704";
        for (arg, path) in [
            ("--server-cert", None),
            ("--server-cert=ca.pem", Some(PathBuf::from("ca.pem"))),
        ] {
            let cli = Cli::try_parse_from(["snapclient-rs", arg, url]).unwrap();
            assert_eq!(cli.url.as_deref(), Some(url));
            assert_eq!(cli.server_certificate, Some(path));
            let err = cli.into_settings().unwrap_err().to_string();
            assert!(err.contains("--server-cert requires TLS"), "{err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn cli_daemon_priority_must_be_within_documented_range() {
        for arg in ["--daemon=-21", "--daemon=20"] {
            let cli = Cli::try_parse_from(["snapclient-rs", arg, "tcp://server:1704"]).unwrap();
            let err = cli.into_settings().unwrap_err();
            assert!(err.to_string().contains("invalid daemon priority"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn cli_daemon_preserves_the_url_and_accepts_priority_boundaries() {
        for (arg, priority) in [("--daemon", -3), ("--daemon=-20", -20), ("--daemon=19", 19)] {
            let settings = Cli::try_parse_from(["snapclient-rs", arg, "tcp://server:1800"])
                .unwrap()
                .into_settings()
                .unwrap();
            assert_eq!(settings.server.host, "server");
            assert_eq!(settings.server.port, 1800);
            assert_eq!(settings.daemon.unwrap().priority, Some(priority));
        }
    }

    #[cfg(unix)]
    #[test]
    fn cli_rejects_the_unsupported_daemon_user_override() {
        let err = Cli::try_parse_from(["snapclient-rs", "--daemon", "--user", "example"])
            .unwrap()
            .into_settings()
            .unwrap_err();
        assert!(err.to_string().contains("--user is not supported"));
    }
}
