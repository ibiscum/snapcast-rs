//! Client configuration types mirroring the C++ `ClientSettings`.

use std::path::PathBuf;

use snapcast_proto::SampleFormat;

/// Server connection settings.
#[derive(Debug, Clone)]
pub struct ServerSettings {
    /// Connection scheme.
    ///
    /// Snapcast audio streaming currently supports only `"tcp"`.
    /// Other schemes may be accepted by higher-level parsers for future
    /// compatibility, but the runtime transport rejects unsupported schemes.
    pub scheme: String,
    /// Server hostname or IP.
    pub host: String,
    /// Server port.
    pub port: u16,
    /// Optional authentication.
    pub auth: Option<Auth>,
    /// Server CA certificate for TLS verification.
    ///
    /// Kept in the shared settings model for CLI/config compatibility. It is
    /// only consumed when TLS support is compiled and selected by the caller.
    pub server_certificate: Option<PathBuf>,
    /// Client certificate (PEM).
    ///
    /// Kept in the shared settings model for CLI/config compatibility. It is
    /// only consumed when TLS support is compiled and selected by the caller.
    pub certificate: Option<PathBuf>,
    /// Client private key (PEM).
    ///
    /// Kept in the shared settings model for CLI/config compatibility. It is
    /// only consumed when TLS support is compiled and selected by the caller.
    pub certificate_key: Option<PathBuf>,
    /// Password for encrypted private key.
    ///
    /// Kept in the shared settings model for CLI/config compatibility. It is
    /// only consumed when TLS support is compiled and selected by the caller.
    pub key_password: Option<String>,
}

/// Authentication credentials.
#[derive(Debug, Clone)]
pub struct Auth {
    /// Auth scheme (e.g. "Basic").
    pub scheme: String,
    /// Auth parameter (e.g. base64-encoded credentials).
    pub param: String,
}

/// Mixer mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MixerMode {
    /// Software volume control (default).
    #[default]
    Software,
    /// Hardware mixer (e.g. ALSA).
    Hardware,
    /// External script mixer.
    Script,
    /// No volume control.
    None,
}

/// Mixer settings.
#[derive(Debug, Clone, Default)]
pub struct MixerSettings {
    /// Mixer mode.
    pub mode: MixerMode,
    /// Mixer-specific parameter string.
    pub parameter: String,
}

/// PCM device identifier.
#[derive(Debug, Clone)]
pub struct PcmDevice {
    /// Device index (-1 for default).
    pub idx: i32,
    /// Device name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
}

impl Default for PcmDevice {
    fn default() -> Self {
        Self {
            idx: -1,
            name: "default".into(),
            description: String::new(),
        }
    }
}

/// Audio player settings.
#[derive(Debug, Clone, Default)]
pub struct PlayerSettings {
    /// Backend name (e.g. "alsa", "coreaudio").
    pub player_name: String,
    /// Backend-specific parameters.
    pub parameter: String,
    /// Additional latency in milliseconds.
    pub latency: i32,
    /// PCM output device.
    pub pcm_device: PcmDevice,
    /// Requested sample format (default: server format).
    pub sample_format: SampleFormat,
    /// Volume mixer settings.
    pub mixer: MixerSettings,
}

/// Logging settings.
#[derive(Debug, Clone)]
pub struct LoggingSettings {
    /// Log sink: "stdout", "stderr", "null", "file:\<path\>".
    pub sink: String,
    /// Log filter: "\<tag\>:\<level\>[,...]".
    pub filter: String,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            sink: "stdout".into(),
            filter: "*:info".into(),
        }
    }
}

/// Daemon settings (Unix only).
#[cfg(unix)]
#[derive(Debug, Clone, Default)]
pub struct DaemonSettings {
    /// Process priority [-20..19], None if not daemonizing.
    pub priority: Option<i32>,
    /// `User[:group]` to run as.
    pub user: Option<String>,
}

/// Top-level client settings.
#[derive(Debug, Clone)]
pub struct ClientSettings {
    /// Instance id when running multiple clients on one host.
    pub instance: u32,
    /// Unique host identifier (default: MAC address).
    pub host_id: String,
    /// Server connection settings.
    ///
    /// Defaults to an empty host so higher-level callers (for example CLI
    /// mDNS discovery) can populate the target address before connecting.
    pub server: ServerSettings,
    /// Audio player settings.
    pub player: PlayerSettings,
    /// Logging configuration.
    pub logging: LoggingSettings,
    /// Daemon settings (Unix only).
    #[cfg(unix)]
    pub daemon: Option<DaemonSettings>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            scheme: snapcast_proto::SCHEME_TCP.into(),
            host: String::new(),
            port: snapcast_proto::DEFAULT_STREAM_PORT,
            auth: None,
            server_certificate: None,
            certificate: None,
            certificate_key: None,
            key_password: None,
        }
    }
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            instance: 1,
            host_id: String::new(),
            server: ServerSettings::default(),
            player: PlayerSettings::default(),
            logging: LoggingSettings::default(),
            #[cfg(unix)]
            daemon: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_settings_default_covers_all_fields() {
        let s = ClientSettings::default();

        assert_eq!(s.instance, 1);
        assert_eq!(s.host_id, "");

        assert_eq!(s.server.scheme, snapcast_proto::SCHEME_TCP);
        assert_eq!(s.server.host, "");
        assert_eq!(s.server.port, snapcast_proto::DEFAULT_STREAM_PORT);
        assert!(s.server.auth.is_none());
        assert!(s.server.server_certificate.is_none());
        assert!(s.server.certificate.is_none());
        assert!(s.server.certificate_key.is_none());
        assert!(s.server.key_password.is_none());

        assert_eq!(s.player.player_name, "");
        assert_eq!(s.player.parameter, "");
        assert_eq!(s.player.latency, 0);
        assert_eq!(s.player.pcm_device.idx, -1);
        assert_eq!(s.player.pcm_device.name, "default");
        assert_eq!(s.player.pcm_device.description, "");
        assert_eq!(s.player.sample_format, SampleFormat::default());
        assert_eq!(s.player.mixer.mode, MixerMode::Software);
        assert_eq!(s.player.mixer.parameter, "");

        assert_eq!(s.logging.sink, "stdout");
        assert_eq!(s.logging.filter, "*:info");

        #[cfg(unix)]
        assert!(s.daemon.is_none());
    }

    #[test]
    fn client_settings_default_keeps_host_unset_for_discovery_or_explicit_config() {
        let s = ClientSettings::default();
        assert!(s.server.host.is_empty());
    }

    #[test]
    fn server_settings_default_matches_proto_constants() {
        let s = ServerSettings::default();
        assert_eq!(s.scheme, snapcast_proto::SCHEME_TCP);
        assert_eq!(s.port, snapcast_proto::DEFAULT_STREAM_PORT);
    }

    #[test]
    fn mixer_and_pcm_defaults_are_stable() {
        let mixer = MixerSettings::default();
        assert_eq!(mixer.mode, MixerMode::Software);
        assert_eq!(mixer.parameter, "");

        let pcm = PcmDevice::default();
        assert_eq!(pcm.idx, -1);
        assert_eq!(pcm.name, "default");
        assert_eq!(pcm.description, "");
    }

    #[test]
    fn clone_preserves_non_default_settings() {
        let original = ClientSettings {
            instance: 7,
            host_id: "host-xyz".into(),
            server: ServerSettings {
                scheme: snapcast_proto::SCHEME_TCP.into(),
                host: "server.local".into(),
                port: 1705,
                auth: Some(Auth {
                    scheme: "Basic".into(),
                    param: "dXNlcjpwYXNz".into(),
                }),
                server_certificate: Some(PathBuf::from("/etc/ssl/ca.pem")),
                certificate: Some(PathBuf::from("/etc/ssl/client.pem")),
                certificate_key: Some(PathBuf::from("/etc/ssl/client.key")),
                key_password: Some("secret".into()),
            },
            player: PlayerSettings {
                player_name: "alsa".into(),
                parameter: "buffer_time=100".into(),
                latency: 42,
                pcm_device: PcmDevice {
                    idx: 2,
                    name: "hw:2".into(),
                    description: "USB DAC".into(),
                },
                sample_format: "48000:16:2".parse().expect("parse sample format"),
                mixer: MixerSettings {
                    mode: MixerMode::Hardware,
                    parameter: "hw:2".into(),
                },
            },
            logging: LoggingSettings {
                sink: "stderr".into(),
                filter: "*:debug".into(),
            },
            #[cfg(unix)]
            daemon: Some(DaemonSettings {
                priority: Some(-5),
                user: Some("snapcast:snapcast".into()),
            }),
        };

        let cloned = original.clone();

        assert_eq!(cloned.instance, original.instance);
        assert_eq!(cloned.host_id, original.host_id);
        assert_eq!(cloned.server.scheme, original.server.scheme);
        assert_eq!(cloned.server.host, original.server.host);
        assert_eq!(cloned.server.port, original.server.port);
        let cloned_auth = cloned.server.auth.expect("missing auth after clone");
        let original_auth = original.server.auth.expect("missing auth in original");
        assert_eq!(cloned_auth.scheme, original_auth.scheme);
        assert_eq!(cloned_auth.param, original_auth.param);
        assert_eq!(
            cloned.server.server_certificate,
            original.server.server_certificate
        );
        assert_eq!(cloned.server.certificate, original.server.certificate);
        assert_eq!(cloned.server.certificate_key, original.server.certificate_key);
        assert_eq!(cloned.server.key_password, original.server.key_password);
        assert_eq!(cloned.player.player_name, original.player.player_name);
        assert_eq!(cloned.player.parameter, original.player.parameter);
        assert_eq!(cloned.player.latency, original.player.latency);
        assert_eq!(cloned.player.pcm_device.idx, original.player.pcm_device.idx);
        assert_eq!(cloned.player.pcm_device.name, original.player.pcm_device.name);
        assert_eq!(
            cloned.player.pcm_device.description,
            original.player.pcm_device.description
        );
        assert_eq!(cloned.player.sample_format, original.player.sample_format);
        assert_eq!(cloned.player.mixer.mode, original.player.mixer.mode);
        assert_eq!(cloned.player.mixer.parameter, original.player.mixer.parameter);
        assert_eq!(cloned.logging.sink, original.logging.sink);
        assert_eq!(cloned.logging.filter, original.logging.filter);
        #[cfg(unix)]
        assert_eq!(
            cloned.daemon.as_ref().and_then(|d| d.priority),
            original.daemon.as_ref().and_then(|d| d.priority)
        );
        #[cfg(unix)]
        assert_eq!(
            cloned.daemon.as_ref().and_then(|d| d.user.clone()),
            original.daemon.as_ref().and_then(|d| d.user.clone())
        );
    }
}
