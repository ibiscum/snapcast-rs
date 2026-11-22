#![deny(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast client library — embeddable synchronized multiroom audio client.
//!
//! See also: [`snapcast-server`](https://docs.rs/snapcast-server) for the server library.
//!
//! # Example
//! ```no_run
//! use snapcast_client::{SnapClient, ClientConfig, ClientEvent, ClientCommand};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = ClientConfig::default();
//! let (mut client, mut events, mut audio_rx) = SnapClient::new(config);
//! let cmd = client.command_sender();
//!
//! // React to events in a separate task
//! tokio::spawn(async move {
//!     while let Some(event) = events.recv().await {
//!         match event {
//!             ClientEvent::VolumeChanged { volume, muted } => {
//!                 println!("Volume: {volume}, muted: {muted}");
//!             }
//!             _ => {}
//!         }
//!     }
//! });
//!
//! // Drain decoded audio as it arrives.
//! tokio::spawn(async move {
//!     while let Some(frame) = audio_rx.recv().await {
//!         // Pass the frame to your audio output pipeline.
//!         let _ = frame;
//!     }
//! });
//!
//! // Stop on Ctrl-C
//! let stop = cmd.clone();
//! tokio::spawn(async move {
//!     tokio::signal::ctrl_c().await.ok();
//!     stop.send(ClientCommand::Stop).await.ok();
//! });
//!
//! client.run().await?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod connection;
pub(crate) mod controller;
#[cfg(feature = "encryption")]
pub(crate) mod crypto;
pub mod decoder;
pub(crate) mod double_buffer;
pub mod stream;
pub mod time_provider;

#[cfg(feature = "resampler")]
pub mod resampler;

use tokio::sync::mpsc;

const EVENT_CHANNEL_SIZE: usize = 256;
const COMMAND_CHANNEL_SIZE: usize = 64;
const AUDIO_CHANNEL_SIZE: usize = 256;

// Re-export proto types that embedders need
#[cfg(feature = "custom-protocol")]
pub use snapcast_proto::CustomMessage;
pub use snapcast_proto::SampleFormat;
pub use snapcast_proto::{DEFAULT_STREAM_PORT, PROTOCOL_VERSION};

/// Interleaved f32 audio frame produced by the client library.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Interleaved f32 samples (channel-interleaved).
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
    /// Server timestamp in microseconds (for sync).
    pub timestamp_usec: i64,
}

/// Events emitted by the client to the consumer.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Connected to server.
    Connected {
        /// Server hostname or IP.
        host: String,
        /// Server port.
        port: u16,
    },
    /// Disconnected from server.
    Disconnected {
        /// Reason for disconnection.
        reason: String,
    },
    /// Audio stream started with the given format.
    StreamStarted {
        /// Codec name (e.g. "flac", "opus").
        codec: String,
        /// PCM sample format.
        format: SampleFormat,
    },
    /// Server settings received or updated.
    ServerSettings {
        /// Playout buffer in milliseconds.
        buffer_ms: i32,
        /// Additional latency in milliseconds.
        latency: i32,
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Volume changed (from server or local).
    VolumeChanged {
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// The initial time probes have finished and at least one time response
    /// has been processed for this connection.
    TimeSyncComplete {
        /// Clock difference to server in milliseconds.
        diff_ms: f64,
    },
    #[cfg(feature = "custom-protocol")]
    /// Custom message received from server.
    CustomMessage(snapcast_proto::CustomMessage),
}

/// Commands the consumer sends to the client.
#[derive(Debug, Clone)]
pub enum ClientCommand {
    /// Set volume (0–100) and mute state.
    SetVolume {
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Send a custom message to the server.
    #[cfg(feature = "custom-protocol")]
    SendCustom(snapcast_proto::CustomMessage),
    /// Stop the client gracefully, including during connection setup or retry.
    Stop,
}

/// Configuration for the embeddable client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Connection scheme (`tcp`, `ws`, or `wss`).
    pub scheme: String,
    /// Server hostname or IP.
    pub host: String,
    /// Server port. Default: 1704.
    pub port: u16,
    /// Optional authentication for Hello handshake.
    pub auth: Option<crate::config::Auth>,
    /// Server CA certificate for WSS server verification.
    #[cfg(feature = "tls")]
    pub server_certificate: Option<std::path::PathBuf>,
    /// Client certificate chain (PEM) for WSS client authentication.
    #[cfg(feature = "tls")]
    pub certificate: Option<std::path::PathBuf>,
    /// Client private key (PEM) for WSS client authentication.
    #[cfg(feature = "tls")]
    pub certificate_key: Option<std::path::PathBuf>,
    /// Password for encrypted private key.
    /// Encrypted PEM keys are currently rejected by the WSS transport.
    #[cfg(feature = "tls")]
    pub key_password: Option<String>,
    /// Instance id (for multiple clients on one host).
    pub instance: u32,
    /// Unique host identifier (default: MAC address).
    pub host_id: String,
    /// Additional latency in milliseconds (subtracted from buffer).
    pub latency: i32,
    /// Client name sent in Hello. Default: "Snapclient".
    pub client_name: String,
    /// Pre-shared key for encrypted f32lz4 streams. `None` rejects encrypted
    /// streams; the library does not read a key from the environment.
    #[cfg(feature = "encryption")]
    pub encryption_psk: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            scheme: snapcast_proto::SCHEME_TCP.into(),
            host: "localhost".into(),
            port: snapcast_proto::DEFAULT_STREAM_PORT,
            auth: None,
            #[cfg(feature = "tls")]
            server_certificate: None,
            #[cfg(feature = "tls")]
            certificate: None,
            #[cfg(feature = "tls")]
            certificate_key: None,
            #[cfg(feature = "tls")]
            key_password: None,
            instance: 1,
            host_id: String::new(),
            latency: 0,
            client_name: snapcast_proto::DEFAULT_CLIENT_NAME.into(),
            #[cfg(feature = "encryption")]
            encryption_psk: None,
        }
    }
}

/// The embeddable Snapcast client.
pub struct SnapClient {
    config: ClientConfig,
    event_tx: mpsc::Sender<ClientEvent>,
    command_tx: mpsc::Sender<ClientCommand>,
    command_rx: Option<mpsc::Receiver<ClientCommand>>,
    audio_tx: mpsc::Sender<AudioFrame>,
    /// Shared time provider — accessible by the binary for audio output.
    pub time_provider: std::sync::Arc<std::sync::Mutex<time_provider::TimeProvider>>,
    /// Shared stream — accessible by the binary for audio output.
    pub stream: std::sync::Arc<std::sync::Mutex<stream::Stream>>,
}

impl SnapClient {
    /// Create a new client. Returns the client, event receiver, and audio output receiver.
    ///
    /// Both output channels are bounded. The client drops new events or audio
    /// frames when the corresponding receiver cannot keep up.
    pub fn new(
        config: ClientConfig,
    ) -> (
        Self,
        mpsc::Receiver<ClientEvent>,
        mpsc::Receiver<AudioFrame>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_SIZE);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_SIZE);
        let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_SIZE);
        let time_provider =
            std::sync::Arc::new(std::sync::Mutex::new(time_provider::TimeProvider::new()));
        let stream = std::sync::Arc::new(std::sync::Mutex::new(stream::Stream::new(
            SampleFormat::default(),
        )));
        let client = Self {
            config,
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            audio_tx,
            time_provider,
            stream,
        };
        (client, event_rx, audio_rx)
    }

    /// Get a cloneable command sender.
    ///
    /// While connecting or retrying, non-stop commands are deferred in FIFO
    /// order. If that backlog fills, new non-stop commands are dropped so that
    /// [`ClientCommand::Stop`] can still be handled.
    pub fn command_sender(&self) -> mpsc::Sender<ClientCommand> {
        self.command_tx.clone()
    }

    /// Run the client, retrying connection failures until stopped.
    ///
    /// This is a one-shot operation. Once polled, the future consumes the run
    /// slot even if startup fails or the future is canceled.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let command_rx = self
            .command_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("run() already called"))?;

        let mut ctrl = controller::Controller::new(
            self.config.clone(),
            self.event_tx.clone(),
            self.audio_tx.clone(),
            std::sync::Arc::clone(&self.time_provider),
            std::sync::Arc::clone(&self.stream),
        )?;
        ctrl.run(command_rx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_proto::MessageType;
    use snapcast_proto::message::base::BaseMessage;
    use snapcast_proto::message::codec_header::CodecHeader;
    use snapcast_proto::message::factory::{self, MessagePayload};
    use snapcast_proto::message::server_settings::ServerSettings;
    use snapcast_proto::message::time::Time;
    use snapcast_proto::message::wire_chunk::WireChunk;
    use snapcast_proto::types::Timeval;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::timeout;
    #[cfg(feature = "websocket")]
    use futures_util::{SinkExt, StreamExt};
    #[cfg(feature = "websocket")]
    use tokio_tungstenite::accept_async;
    #[cfg(feature = "websocket")]
    use tokio_tungstenite::tungstenite::Message;

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    fn loopback_config(listener: &TcpListener) -> ClientConfig {
        ClientConfig {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            ..ClientConfig::default()
        }
    }

    async fn read_message_header(peer: &mut TcpStream) -> BaseMessage {
        let mut bytes = [0; BaseMessage::HEADER_SIZE];
        peer.read_exact(&mut bytes).await.unwrap();
        let header = BaseMessage::read_from(&mut &bytes[..]).unwrap();
        peer.read_exact(&mut vec![0; header.size as usize])
            .await
            .unwrap();
        header
    }

    #[cfg(feature = "websocket")]
    fn loopback_ws_config(listener: &TcpListener) -> ClientConfig {
        ClientConfig {
            scheme: snapcast_proto::SCHEME_WS.into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            ..ClientConfig::default()
        }
    }

    #[cfg(feature = "websocket")]
    async fn read_ws_message_base(
        ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    ) -> BaseMessage {
        let Message::Binary(frame) = ws.next().await.unwrap().unwrap() else {
            panic!("expected binary frame");
        };
        let (header, payload) = frame.split_at(BaseMessage::HEADER_SIZE);
        let base = BaseMessage::read_from(&mut &header[..]).unwrap();
        assert_eq!(payload.len(), base.size as usize);
        base
    }

    #[test]
    fn client_config_defaults_are_stable() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.scheme, snapcast_proto::SCHEME_TCP);
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, snapcast_proto::DEFAULT_STREAM_PORT);
        assert!(cfg.auth.is_none());
        assert_eq!(cfg.instance, 1);
        assert_eq!(cfg.host_id, "");
        assert_eq!(cfg.latency, 0);
        assert_eq!(cfg.client_name, snapcast_proto::DEFAULT_CLIENT_NAME);
        #[cfg(feature = "tls")]
        {
            assert!(cfg.server_certificate.is_none());
            assert!(cfg.certificate.is_none());
            assert!(cfg.certificate_key.is_none());
            assert!(cfg.key_password.is_none());
        }
        #[cfg(feature = "encryption")]
        assert!(cfg.encryption_psk.is_none());
    }

    #[tokio::test]
    async fn command_sender_is_cloneable_and_sends_to_internal_queue() {
        let (mut client, _events, _audio) = SnapClient::new(ClientConfig::default());
        let tx1 = client.command_sender();
        let tx2 = tx1.clone();
        tx1.send(ClientCommand::Stop).await.unwrap();
        tx2.send(ClientCommand::SetVolume {
            volume: 42,
            muted: true,
        })
        .await
        .unwrap();

        let mut rx = client.command_rx.take().unwrap();
        assert!(matches!(rx.recv().await, Some(ClientCommand::Stop)));
        assert!(matches!(
            rx.recv().await,
            Some(ClientCommand::SetVolume {
                volume: 42,
                muted: true
            })
        ));
    }

    #[test]
    fn new_initializes_shared_state() {
        let (client, _events, _audio) = SnapClient::new(ClientConfig::default());
        let diff = client
            .time_provider
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .diff_to_server_usec();
        assert_eq!(diff, 0);
        let fmt = client
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .format();
        assert_eq!(fmt, SampleFormat::default());
    }

    #[tokio::test]
    async fn run_rejects_unknown_scheme() {
        let config = ClientConfig {
            scheme: "gopher".into(),
            host: "localhost".into(),
            port: snapcast_proto::DEFAULT_HTTP_PORT,
            ..ClientConfig::default()
        };
        let (mut client, _events, _audio_rx) = SnapClient::new(config);

        let err = client.run().await.unwrap_err();
        assert!(err.to_string().contains("unsupported scheme"));
    }

    #[tokio::test]
    async fn failed_startup_consumes_run_slot() {
        let config = ClientConfig {
            scheme: "gopher".into(),
            host: "localhost".into(),
            port: snapcast_proto::DEFAULT_HTTP_PORT,
            ..ClientConfig::default()
        };
        let (mut client, _events, _audio_rx) = SnapClient::new(config);

        let err = client.run().await.unwrap_err();
        assert!(err.to_string().contains("unsupported scheme"));
        let err = client.run().await.unwrap_err();
        assert!(err.to_string().contains("run() already called"));
    }

    #[tokio::test]
    async fn stop_during_handshake_consumes_run_slot() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut client, _events, _audio) = SnapClient::new(loopback_config(&listener));
        let cmd = client.command_sender();

        let (result, ()) = timeout(Duration::from_secs(3), async {
            tokio::join!(client.run(), async {
                let (mut peer, _) = listener.accept().await.unwrap();
                assert_eq!(
                    read_message_header(&mut peer).await.msg_type,
                    MessageType::Hello
                );
                cmd.send(ClientCommand::Stop).await.unwrap();
                // Keep the socket open without sending settings.
                assert_eq!(peer.read(&mut [0; 1]).await.unwrap(), 0);
            })
        })
        .await
        .expect("Stop must interrupt the handshake");
        result.unwrap();
        let err = client.run().await.unwrap_err();
        assert!(err.to_string().contains("run() already called"));
    }

    #[tokio::test]
    async fn stop_during_backoff_exits_before_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut client, mut events, _audio) = SnapClient::new(loopback_config(&listener));
        let cmd = client.command_sender();
        let task = tokio::spawn(async move { client.run().await });

        timeout(TEST_TIMEOUT, async {
            let (peer, _) = listener.accept().await.unwrap();
            drop(peer);
            assert!(matches!(
                events.recv().await,
                Some(ClientEvent::Connected { .. })
            ));
            assert!(matches!(
                events.recv().await,
                Some(ClientEvent::Disconnected { .. })
            ));
        })
        .await
        .expect("first connection failure");

        cmd.send(ClientCommand::Stop).await.unwrap();
        timeout(Duration::from_millis(750), task)
            .await
            .expect("Stop must interrupt the one-second backoff")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn time_sync_complete_waits_for_a_time_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut client, mut events, _audio) = SnapClient::new(loopback_config(&listener));
        let cmd = client.command_sender();

        let (result, ()) = timeout(TEST_TIMEOUT, async {
            tokio::join!(client.run(), async {
                let (mut peer, _) = listener.accept().await.unwrap();
                let mut header = read_message_header(&mut peer).await;
                assert_eq!(header.msg_type, MessageType::Hello);
                header.msg_type = MessageType::ServerSettings;
                let settings = MessagePayload::ServerSettings(ServerSettings {
                    buffer_ms: 1000,
                    latency: 0,
                    volume: 100,
                    muted: false,
                });
                peer.write_all(&factory::serialize(&mut header, &settings).unwrap())
                    .await
                    .unwrap();

                // Initial probe, 50 quick probes, then the first regular probe.
                for _ in 0..52 {
                    header = read_message_header(&mut peer).await;
                    assert_eq!(header.msg_type, MessageType::Time);
                }
                assert!(matches!(
                    events.try_recv(),
                    Ok(ClientEvent::Connected { .. })
                ));
                assert!(matches!(
                    events.try_recv(),
                    Ok(ClientEvent::ServerSettings { .. })
                ));
                assert!(matches!(
                    events.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));

                peer.write_all(
                    &factory::serialize(&mut header, &MessagePayload::Time(Time::new())).unwrap(),
                )
                .await
                .unwrap();
                assert!(matches!(
                    events.recv().await,
                    Some(ClientEvent::TimeSyncComplete { .. })
                ));
                cmd.send(ClientCommand::Stop).await.unwrap();
                peer.read_to_end(&mut Vec::new()).await.unwrap();
            })
        })
        .await
        .expect("time sync must complete after a delayed response");
        result.unwrap();
        let err = client.run().await.unwrap_err();
        assert!(err.to_string().contains("run() already called"));
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn ws_time_sync_complete_waits_for_a_time_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut client, mut events, _audio) = SnapClient::new(loopback_ws_config(&listener));
        let cmd = client.command_sender();

        let (result, ()) = timeout(TEST_TIMEOUT, async {
            tokio::join!(client.run(), async {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = accept_async(socket).await.unwrap();

                let mut base = read_ws_message_base(&mut ws).await;
                assert_eq!(base.msg_type, MessageType::Hello);
                base.msg_type = MessageType::ServerSettings;
                let settings = MessagePayload::ServerSettings(ServerSettings {
                    buffer_ms: 1000,
                    latency: 0,
                    volume: 100,
                    muted: false,
                });
                ws.send(Message::Binary(
                    factory::serialize(&mut base, &settings).unwrap().into(),
                ))
                .await
                .unwrap();

                for _ in 0..52 {
                    base = read_ws_message_base(&mut ws).await;
                    assert_eq!(base.msg_type, MessageType::Time);
                }
                assert!(matches!(
                    events.try_recv(),
                    Ok(ClientEvent::Connected { .. })
                ));
                assert!(matches!(
                    events.try_recv(),
                    Ok(ClientEvent::ServerSettings { .. })
                ));
                assert!(matches!(
                    events.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));

                ws.send(Message::Binary(
                    factory::serialize(&mut base, &MessagePayload::Time(Time::new()))
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
                assert!(matches!(
                    events.recv().await,
                    Some(ClientEvent::TimeSyncComplete { .. })
                ));
                cmd.send(ClientCommand::Stop).await.unwrap();
            })
        })
        .await
        .expect("time sync over ws must complete after a delayed response");

        result.unwrap();
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn ws_wirechunk_emits_audio_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut client, mut events, mut audio_rx) = SnapClient::new(loopback_ws_config(&listener));
        let cmd = client.command_sender();

        let (result, ()) = timeout(TEST_TIMEOUT, async {
            tokio::join!(client.run(), async {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = accept_async(socket).await.unwrap();

                let mut base = read_ws_message_base(&mut ws).await;
                assert_eq!(base.msg_type, MessageType::Hello);

                base.msg_type = MessageType::ServerSettings;
                let settings = MessagePayload::ServerSettings(ServerSettings {
                    buffer_ms: 1000,
                    latency: 0,
                    volume: 100,
                    muted: false,
                });
                ws.send(Message::Binary(
                    factory::serialize(&mut base, &settings).unwrap().into(),
                ))
                .await
                .unwrap();

                base.msg_type = MessageType::CodecHeader;
                let codec = MessagePayload::CodecHeader(CodecHeader {
                    codec: "null".into(),
                    payload: vec![],
                });
                ws.send(Message::Binary(factory::serialize(&mut base, &codec).unwrap().into()))
                    .await
                    .unwrap();

                base.msg_type = MessageType::WireChunk;
                let chunk = MessagePayload::WireChunk(WireChunk {
                    timestamp: Timeval::default(),
                    payload: vec![0x00, 0x40, 0x00, 0xC0],
                });
                ws.send(Message::Binary(factory::serialize(&mut base, &chunk).unwrap().into()))
                    .await
                    .unwrap();

                let stream_started = timeout(Duration::from_secs(2), async {
                    loop {
                        if let Some(ClientEvent::StreamStarted { codec, .. }) = events.recv().await {
                            return codec;
                        }
                    }
                })
                .await
                .unwrap();
                assert_eq!(stream_started, "null");

                let frame = timeout(Duration::from_secs(2), audio_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(frame.channels, 2);
                assert_eq!(frame.sample_rate, 48_000);
                assert_eq!(frame.samples.len(), 2);
                cmd.send(ClientCommand::Stop).await.unwrap();
            })
        })
        .await
        .expect("ws audio frame test timed out");

        result.unwrap();
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn ws_stop_during_backoff_exits_before_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut client, mut events, _audio) = SnapClient::new(loopback_ws_config(&listener));
        let cmd = client.command_sender();
        let task = tokio::spawn(async move { client.run().await });

        timeout(TEST_TIMEOUT, async {
            let (socket, _) = listener.accept().await.unwrap();
            let ws = accept_async(socket).await.unwrap();
            drop(ws);
            assert!(matches!(
                events.recv().await,
                Some(ClientEvent::Connected { .. })
            ));
            assert!(matches!(
                events.recv().await,
                Some(ClientEvent::Disconnected { .. })
            ));
        })
        .await
        .expect("first ws connection failure");

        cmd.send(ClientCommand::Stop).await.unwrap();
        timeout(Duration::from_millis(750), task)
            .await
            .expect("Stop must interrupt the one-second backoff over ws")
            .unwrap()
            .unwrap();
    }
}
