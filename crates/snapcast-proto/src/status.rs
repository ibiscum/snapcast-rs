//! Snapcast status types matching the JSON-RPC wire format.
//!
//! Shared between the embedded server (serialize) and the process backend
//! talking to C++ snapserver (deserialize). Also used by embedders like SnapDog.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Result of `Server.GetStatus`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerStatus {
    /// Full server state.
    pub server: Server,
}

/// Top-level server state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Server {
    /// Server host and version info.
    pub server: ServerInfo,
    /// All groups (each containing its clients).
    pub groups: Vec<Group>,
    /// All configured streams.
    pub streams: Vec<Stream>,
}

/// Server host and software information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Server host details.
    pub host: Host,
    /// Snapserver software info.
    pub snapserver: Snapserver,
}

/// Snapserver software information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapserver {
    /// Software name.
    pub name: String,
    /// Binary protocol version.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    /// JSON-RPC control protocol version.
    #[serde(rename = "controlProtocolVersion")]
    pub control_protocol_version: u32,
    /// Software version string.
    pub version: String,
}

impl Default for Snapserver {
    fn default() -> Self {
        Self {
            name: "snapcast-rs".into(),
            protocol_version: crate::PROTOCOL_VERSION,
            control_protocol_version: crate::CONTROL_PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

// ── Host ──────────────────────────────────────────────────────

/// Host identification and platform info.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Host {
    /// CPU architecture.
    #[serde(default)]
    pub arch: String,
    /// IP address.
    #[serde(default)]
    pub ip: String,
    /// MAC address.
    #[serde(default)]
    pub mac: String,
    /// Hostname.
    #[serde(default)]
    pub name: String,
    /// Operating system.
    #[serde(default)]
    pub os: String,
}

// ── Client ────────────────────────────────────────────────────

/// A Snapcast client (speaker endpoint).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    /// Unique client ID.
    pub id: String,
    /// Whether currently connected.
    pub connected: bool,
    /// Client configuration (persisted).
    pub config: ClientConfig,
    /// Host information.
    pub host: Host,
    /// Snapclient software info.
    #[serde(default)]
    pub snapclient: Snapclient,
    /// Last-seen timestamp.
    #[serde(default, rename = "lastSeen")]
    pub last_seen: LastSeen,
}

/// Client configuration (persisted across restarts).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Multi-instance identifier.
    #[serde(default)]
    pub instance: u32,
    /// Additional latency in milliseconds.
    pub latency: i32,
    /// Display name.
    pub name: String,
    /// Volume settings.
    pub volume: Volume,
}

/// Volume state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Volume {
    /// Mute state.
    pub muted: bool,
    /// Volume percentage (0–100).
    pub percent: u16,
}

/// Snapclient software information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapclient {
    /// Software name.
    #[serde(default)]
    pub name: String,
    /// Protocol version.
    #[serde(default, rename = "protocolVersion")]
    pub protocol_version: u32,
    /// Software version string.
    #[serde(default)]
    pub version: String,
}

/// Last-seen timestamp (seconds + microseconds since epoch).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastSeen {
    /// Seconds since epoch.
    #[serde(default)]
    pub sec: u64,
    /// Microseconds.
    #[serde(default)]
    pub usec: u64,
}

// ── Group ─────────────────────────────────────────────────────

/// A group of clients sharing the same stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Group {
    /// Unique group ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Stream ID this group is playing.
    pub stream_id: String,
    /// Group mute state.
    pub muted: bool,
    /// Clients in this group.
    pub clients: Vec<Client>,
}

// ── Stream ────────────────────────────────────────────────────

/// An audio stream source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stream {
    /// Stream ID.
    pub id: String,
    /// Stream properties (MPRIS-style metadata).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<StreamProperties>,
    /// Playback status.
    pub status: StreamStatus,
    /// Source URI.
    pub uri: StreamUri,
}

/// Stream playback status.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StreamStatus {
    /// No audio data flowing.
    #[default]
    Idle,
    /// Audio data actively streaming.
    Playing,
    /// Stream disabled by configuration.
    Disabled,
    /// Status not recognized.
    ///
    /// Note: serde enum deserialization remains strict; unknown wire strings
    /// currently error unless custom deserialization is added. This variant is
    /// primarily used by manual conversions such as [`From<&str>`].
    Unknown,
}

impl From<&str> for StreamStatus {
    fn from(s: &str) -> Self {
        match s {
            "playing" => Self::Playing,
            "idle" => Self::Idle,
            "disabled" => Self::Disabled,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

        fn sample_status() -> ServerStatus {
            ServerStatus {
                server: Server {
                    server: ServerInfo {
                        host: Host {
                            arch: "x86_64".into(),
                            ip: "127.0.0.1".into(),
                            mac: "aa:bb:cc:dd:ee:ff".into(),
                            name: "snap-host".into(),
                            os: "Linux".into(),
                        },
                        snapserver: Snapserver::default(),
                    },
                    groups: vec![Group {
                        id: "group-1".into(),
                        name: "Main".into(),
                        stream_id: "stream-1".into(),
                        muted: false,
                        clients: vec![Client {
                            id: "client-1".into(),
                            connected: true,
                            config: ClientConfig {
                                instance: 1,
                                latency: 0,
                                name: "Living Room".into(),
                                volume: Volume {
                                    muted: false,
                                    percent: 55,
                                },
                            },
                            host: Host {
                                arch: "x86_64".into(),
                                ip: "127.0.0.2".into(),
                                mac: "11:22:33:44:55:66".into(),
                                name: "speaker".into(),
                                os: "Linux".into(),
                            },
                            snapclient: Snapclient {
                                name: "snapclient".into(),
                                protocol_version: crate::PROTOCOL_VERSION,
                                version: "1.0.0".into(),
                            },
                            last_seen: LastSeen { sec: 1, usec: 2 },
                        }],
                    }],
                    streams: vec![Stream {
                        id: "stream-1".into(),
                        properties: Some(StreamProperties {
                            playback_status: Some("Playing".into()),
                            loop_status: Some("None".into()),
                            shuffle: Some(false),
                            volume: Some(42),
                            mute: Some(false),
                            rate: Some(1.0),
                            position: Some(12.5),
                            can_go_next: true,
                            can_go_previous: true,
                            can_play: true,
                            can_pause: true,
                            can_seek: true,
                            can_control: true,
                            metadata: Some(serde_json::json!({"title":"Song"})),
                        }),
                        status: StreamStatus::Playing,
                        uri: StreamUri {
                            fragment: "frag".into(),
                            host: "localhost".into(),
                            path: "/music".into(),
                            query: HashMap::from([(String::from("codec"), String::from("flac"))]),
                            raw: "tcp://localhost/music?codec=flac#frag".into(),
                            scheme: "tcp".into(),
                        },
                    }],
                },
            }
        }

        #[test]
        fn server_status_round_trip_json() {
            let status = sample_status();
            let json = serde_json::to_string(&status).unwrap();
            let decoded: ServerStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded.server.streams.len(), 1);
            assert_eq!(decoded.server.groups.len(), 1);
            assert_eq!(decoded.server.streams[0].status, StreamStatus::Playing);
            assert_eq!(
                decoded.server.streams[0]
                    .properties
                    .as_ref()
                    .unwrap()
                    .playback_status
                    .as_deref(),
                Some("Playing")
            );
        }

        #[test]
        fn stream_status_from_str_maps_known_and_unknown() {
            assert_eq!(StreamStatus::from("playing"), StreamStatus::Playing);
            assert_eq!(StreamStatus::from("idle"), StreamStatus::Idle);
            assert_eq!(StreamStatus::from("disabled"), StreamStatus::Disabled);
            assert_eq!(StreamStatus::from("paused"), StreamStatus::Unknown);
        }

        #[test]
        fn serde_unknown_stream_status_is_error() {
            let stream = r#"{
                "id":"s1",
                "status":"paused",
                "uri":{"raw":"tcp://localhost","scheme":"tcp"}
            }"#;
            assert!(serde_json::from_str::<Stream>(stream).is_err());
        }

        #[test]
        fn missing_defaulted_fields_are_filled() {
            let client = r#"{
                "id":"c1",
                "connected":true,
                "config":{"latency":0,"name":"N","volume":{"muted":false,"percent":50}},
                "host":{}
            }"#;
            let decoded: Client = serde_json::from_str(client).unwrap();
            assert_eq!(decoded.snapclient.name, "");
            assert_eq!(decoded.snapclient.protocol_version, 0);
            assert_eq!(decoded.last_seen.sec, 0);
            assert_eq!(decoded.last_seen.usec, 0);
            assert_eq!(decoded.config.instance, 0);
            assert_eq!(decoded.host.name, "");
        }

        #[test]
        fn missing_required_fields_fail() {
            let stream_missing_status = r#"{
                "id":"s1",
                "uri":{"raw":"tcp://localhost","scheme":"tcp"}
            }"#;
            assert!(serde_json::from_str::<Stream>(stream_missing_status).is_err());

            let stream_uri_missing_raw = r#"{
                "id":"s1",
                "status":"idle",
                "uri":{"scheme":"tcp"}
            }"#;
            assert!(serde_json::from_str::<Stream>(stream_uri_missing_raw).is_err());
        }

        #[test]
        fn unknown_fields_are_ignored_for_forward_compatibility() {
            let stream = r#"{
                "id":"s1",
                "status":"idle",
                "uri":{"raw":"tcp://localhost","scheme":"tcp"},
                "extra":"ignored"
            }"#;
            let decoded: Stream = serde_json::from_str(stream).unwrap();
            assert_eq!(decoded.id, "s1");
            assert_eq!(decoded.status, StreamStatus::Idle);
            assert_eq!(decoded.uri.raw, "tcp://localhost");
        }

        #[test]
        fn volume_values_are_preserved_at_proto_layer() {
            let stream_props = r#"{
                "playback_status":"Playing",
                "volume":65535
            }"#;
            let props: StreamProperties = serde_json::from_str(stream_props).unwrap();
            assert_eq!(props.volume, Some(u16::MAX));
        }
    }

/// Parsed stream URI components.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUri {
    /// URI fragment.
    #[serde(default)]
    pub fragment: String,
    /// Host component.
    #[serde(default)]
    pub host: String,
    /// Path component.
    #[serde(default)]
    pub path: String,
    /// Query parameters.
    #[serde(default)]
    pub query: HashMap<String, String>,
    /// Raw URI string.
    pub raw: String,
    /// URI scheme (pipe, tcp, process, etc.).
    #[serde(default)]
    pub scheme: String,
}

/// Stream properties (MPRIS-style metadata and capabilities).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamProperties {
    /// Playback status (Playing, Paused, Stopped).
    #[serde(default)]
    pub playback_status: Option<String>,
    /// Loop status (None, Track, Playlist).
    #[serde(default)]
    pub loop_status: Option<String>,
    /// Shuffle mode.
    #[serde(default)]
    pub shuffle: Option<bool>,
    /// Volume (0–100).
    #[serde(default)]
    pub volume: Option<u16>,
    /// Mute state.
    #[serde(default)]
    pub mute: Option<bool>,
    /// Playback rate.
    #[serde(default)]
    pub rate: Option<f64>,
    /// Position in seconds.
    #[serde(default)]
    pub position: Option<f64>,
    /// Can skip to next track.
    #[serde(default)]
    pub can_go_next: bool,
    /// Can skip to previous track.
    #[serde(default)]
    pub can_go_previous: bool,
    /// Can start playback.
    #[serde(default)]
    pub can_play: bool,
    /// Can pause playback.
    #[serde(default)]
    pub can_pause: bool,
    /// Can seek within track.
    #[serde(default)]
    pub can_seek: bool,
    /// Can control playback at all.
    #[serde(default)]
    pub can_control: bool,
    /// Track metadata (artist, title, album, etc.).
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}
