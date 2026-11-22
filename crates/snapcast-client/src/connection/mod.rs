//! Connection layer.
//!
//! TCP is the supported Snapcast audio transport. The WebSocket modules are
//! kept feature-gated for future interoperability work, but they are not
//! selected by [`SnapConnection::new`] until the server and client can speak a
//! verified binary audio-streaming WebSocket contract.

#[cfg(feature = "websocket")]
pub mod ws;
#[cfg(feature = "tls")]
pub mod wss;

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context, Result};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_BUFFERED_MESSAGES: usize = 64;

#[derive(Default)]
struct FrameReader {
    header: [u8; BaseMessage::HEADER_SIZE],
    header_read: usize,
    payload: Option<IncomingPayload>,
}

struct IncomingPayload {
    base: BaseMessage,
    bytes: Vec<u8>,
    read: usize,
}

impl FrameReader {
    async fn read_frame(
        &mut self,
        reader: &mut (impl AsyncReadExt + Unpin),
    ) -> Result<TypedMessage> {
        if self.payload.is_none() {
            read_remaining(reader, &mut self.header, &mut self.header_read)
                .await
                .context("reading base message header")?;
            self.header_read = 0;
            let mut base =
                BaseMessage::read_from(&mut &self.header[..]).context("parsing header")?;
            base.received = steady_time_of_day();
            ensure_payload_size(base.size)?;
            self.payload = Some(IncomingPayload {
                bytes: vec![0; base.size as usize],
                base,
                read: 0,
            });
        }

        let payload = self.payload.as_mut().expect("header has been read");
        read_remaining(reader, &mut payload.bytes, &mut payload.read)
            .await
            .context("reading payload")?;
        let payload = self.payload.take().expect("payload has been read");
        factory::deserialize(payload.base, &payload.bytes).context("deserializing frame")
    }
}

async fn read_remaining(
    reader: &mut (impl AsyncReadExt + Unpin),
    buffer: &mut [u8],
    read: &mut usize,
) -> std::io::Result<()> {
    // `read` is cancellation-safe; keep the completed byte count outside the future.
    while *read < buffer.len() {
        let count = reader.read(&mut buffer[*read..]).await?;
        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        *read += count;
    }
    Ok(())
}

// Used by the in-memory and loopback framing tests.
#[cfg(test)]
async fn read_frame(reader: &mut (impl AsyncReadExt + Unpin)) -> Result<TypedMessage> {
    FrameReader::default().read_frame(reader).await
}

pub(crate) fn ensure_payload_size(size: u32) -> Result<()> {
    anyhow::ensure!(
        size <= snapcast_proto::DEFAULT_MAX_PAYLOAD_SIZE,
        "payload too large: {size} bytes"
    );
    Ok(())
}

#[cfg(test)]
async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    base: &mut BaseMessage,
    payload: &MessagePayload,
) -> Result<()> {
    let frame =
        factory::serialize(base, payload).map_err(|e| anyhow::anyhow!("serializing: {e}"))?;
    writer.write_all(&frame).await.context("writing frame")?;
    Ok(())
}

/// TCP connection to a snapserver.
pub struct TcpConnection {
    stream: Option<TcpStream>,
    host: String,
    port: u16,
    reader: FrameReader,
    buffered: VecDeque<TypedMessage>,
    next_id: u16,
}

/// Unified connection over supported transports.
pub enum SnapConnection {
    /// Plain TCP connection.
    Tcp(TcpConnection),
    #[cfg(feature = "websocket")]
    /// WebSocket (non-secure) connection.
    Ws(ws::WsConnection),
    #[cfg(feature = "tls")]
    /// WebSocket over TLS (secure) connection.
    Wss(wss::WssConnection),
}

impl SnapConnection {
    /// Create a new connection based on the scheme.
    pub fn new(scheme: &str, host: &str, port: u16) -> Result<Self> {
        match scheme {
            snapcast_proto::SCHEME_TCP => Ok(Self::Tcp(TcpConnection::new(host, port))),
            snapcast_proto::SCHEME_WS | snapcast_proto::SCHEME_WSS => anyhow::bail!(
                "websocket audio transport is not supported yet; use tcp:// for Snapcast audio"
            ),
            _ => anyhow::bail!("unsupported scheme: {scheme}"),
        }
    }

    /// Establish the connection.
    pub async fn connect(&mut self) -> Result<()> {
        match self {
            Self::Tcp(c) => c.connect().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.connect().await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.connect().await,
        }
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        match self {
            Self::Tcp(c) => c.disconnect(),
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.disconnect(),
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.disconnect(),
        }
    }

    /// Send a message.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        match self {
            Self::Tcp(c) => c.send(msg_type, payload).await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.send(msg_type, payload).await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.send(msg_type, payload).await,
        }
    }

    /// Receive the next message.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        match self {
            Self::Tcp(c) => c.recv().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.recv().await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.recv().await,
        }
    }
}

impl TcpConnection {
    /// Create a new connection to the given host and port.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            stream: None,
            host: host.to_string(),
            port,
            reader: FrameReader::default(),
            buffered: VecDeque::new(),
            next_id: 1,
        }
    }

    /// Establish the TCP connection.
    pub async fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = TcpStream::connect(&addr)
            .await
            .with_context(|| format!("connecting to {addr}"))?;
        self.disconnect();
        self.stream = Some(stream);
        self.next_id = 1;
        Ok(())
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        self.stream = None;
        self.reader = FrameReader::default();
        self.buffered.clear();
    }

    /// Send a message without waiting for a response.
    ///
    /// Canceling an in-progress write closes the connection.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        self.send_with_id(msg_type, payload, 0).await
    }

    async fn send_with_id(
        &mut self,
        msg_type: MessageType,
        payload: &MessagePayload,
        id: u16,
    ) -> Result<()> {
        anyhow::ensure!(self.stream.is_some(), "not connected");
        let mut base = BaseMessage {
            msg_type,
            id,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        stamp_sent(&mut base);
        let frame = factory::serialize(&mut base, payload).context("serializing frame")?;
        // A canceled or failed write may leave a partial frame on the wire.
        let mut stream = self.stream.take().context("not connected")?;
        stream.write_all(&frame).await.context("writing frame")?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Send a request and wait for the response (matched by `refersTo`).
    ///
    /// The timeout covers sending and waiting for the reply. Other messages remain
    /// available to [`Self::recv`] in arrival order. If the backlog fills, drain
    /// it with `recv` before sending another request. A reply received after the
    /// request was canceled or timed out is returned by `recv` like any other message.
    pub async fn send_request(
        &mut self,
        msg_type: MessageType,
        payload: &MessagePayload,
        timeout: Duration,
    ) -> Result<TypedMessage> {
        tokio::time::timeout(timeout, async {
            self.ensure_backlog_space()?;
            let id = self.next_request_id();
            self.send_with_id(msg_type, payload, id).await?;
            loop {
                self.ensure_backlog_space()?;
                let msg = self.recv_from_stream().await?;
                if msg.base.refers_to == id {
                    return Ok(msg);
                }
                self.buffered.push_back(msg);
            }
        })
        .await
        .context("request timed out")?
    }

    fn ensure_backlog_space(&self) -> Result<()> {
        anyhow::ensure!(
            self.buffered.len() < MAX_BUFFERED_MESSAGES,
            "request message backlog full; drain it with recv"
        );
        Ok(())
    }

    /// Receive the next message, starting with messages buffered by a request.
    ///
    /// Canceling this future preserves any partially received TCP frame.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        if let Some(msg) = self.buffered.pop_front() {
            return Ok(msg);
        }
        self.recv_from_stream().await
    }

    async fn recv_from_stream(&mut self) -> Result<TypedMessage> {
        let stream = self.stream.as_mut().context("not connected")?;
        let result = self.reader.read_frame(stream).await;
        if result.is_err() {
            self.stream = None;
            self.reader = FrameReader::default();
        }
        result
    }

    fn next_request_id(&mut self) -> u16 {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        id
    }
}

pub(super) fn stamp_sent(base: &mut BaseMessage) {
    let tv = steady_time_of_day();
    base.sent = tv;
}

/// Matches the C++ `chronos::steadytimeofday` — monotonic clock time.
/// On macOS/Linux this samples the same clock domain as the C++ snapserver.
pub(super) fn steady_time_of_day() -> Timeval {
    let usec = snapcast_proto::time::now_usec();
    Timeval {
        sec: (usec / 1_000_000) as i32,
        usec: (usec % 1_000_000) as i32,
    }
}

/// Current time in microseconds using the steady clock.
///
/// Single source of truth lives in [`snapcast_proto::time`] so the client and
/// server cannot drift onto different clock domains.
pub fn now_usec() -> i64 {
    snapcast_proto::time::now_usec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_proto::message::time::Time;
    use tokio::net::TcpListener;

    /// Test frame read/write with in-memory buffers (no network needed).
    #[tokio::test]
    async fn write_and_read_frame() {
        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 1234 },
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 42,
            refers_to: 0,
            sent: Timeval { sec: 1, usec: 0 },
            received: Timeval { sec: -1, usec: 0 },
            size: 0,
        };

        // Write to buffer
        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();

        // Size should be header + payload
        assert_eq!(buf.len(), BaseMessage::HEADER_SIZE + Time::SIZE as usize);

        // Read back
        let mut cursor = std::io::Cursor::new(&buf);
        let before = now_usec();
        let msg = read_frame(&mut cursor).await.unwrap();
        let after = now_usec();
        assert_eq!(msg.base.msg_type, MessageType::Time);
        assert_eq!(msg.base.id, 42);
        assert_eq!(msg.base.sent, base.sent);
        assert_ne!(msg.base.received, base.received);
        assert!((before..=after).contains(&msg.base.received.to_usec()));
        match msg.payload {
            MessagePayload::Time(t) => assert_eq!(t.latency.usec, 1234),
            _ => panic!("expected Time"),
        }
    }

    #[tokio::test]
    async fn write_and_read_error_frame() {
        use snapcast_proto::message::error::Error;

        let payload = MessagePayload::Error(Error {
            code: 401,
            error: "Unauthorized".into(),
            message: "bad auth".into(),
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Error,
            id: 0,
            refers_to: 7,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };

        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();

        let mut cursor = std::io::Cursor::new(&buf);
        let msg = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg.base.refers_to, 7);
        match msg.payload {
            MessagePayload::Error(e) => {
                assert_eq!(e.code, 401);
                assert_eq!(e.error, "Unauthorized");
            }
            _ => panic!("expected Error"),
        }
    }

    #[tokio::test]
    async fn write_and_read_multiple_frames() {
        let frames: Vec<(MessageType, MessagePayload)> = vec![
            (MessageType::Time, MessagePayload::Time(Time::default())),
            (
                MessageType::ClientInfo,
                MessagePayload::ClientInfo(snapcast_proto::message::client_info::ClientInfo {
                    volume: 80,
                    muted: false,
                }),
            ),
        ];

        let mut buf = Vec::new();
        for (mt, payload) in &frames {
            let mut base = BaseMessage {
                msg_type: *mt,
                id: 0,
                refers_to: 0,
                sent: Timeval::default(),
                received: Timeval::default(),
                size: 0,
            };
            write_frame(&mut buf, &mut base, payload).await.unwrap();
        }

        // Read both back
        let mut cursor = std::io::Cursor::new(&buf);
        let msg1 = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg1.base.msg_type, MessageType::Time);
        let msg2 = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg2.base.msg_type, MessageType::ClientInfo);
    }

    #[test]
    fn tcp_connection_new() {
        let conn = TcpConnection::new("localhost", 1704);
        assert!(conn.stream.is_none());
        assert_eq!(conn.host, "localhost");
        assert_eq!(conn.port, 1704);
    }

    #[tokio::test]
    async fn frame_reader_enforces_payload_size_at_header_boundary() {
        let limit = snapcast_proto::DEFAULT_MAX_PAYLOAD_SIZE;
        let mut base = BaseMessage {
            msg_type: MessageType::StreamTags,
            id: 1,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: limit + 1,
        };
        let mut bytes = Vec::new();
        base.write_to(&mut bytes).unwrap();
        let err = read_frame(&mut bytes.as_slice()).await.unwrap_err();
        assert!(err.to_string().contains("payload too large"));

        bytes.clear();
        base.size = limit;
        base.write_to(&mut bytes).unwrap();
        bytes.resize(BaseMessage::HEADER_SIZE + limit as usize, 42);
        let msg = read_frame(&mut bytes.as_slice()).await.unwrap();
        assert!(
            matches!(msg.payload, MessagePayload::StreamTags(data) if data.len() == limit as usize && data.iter().all(|&b| b == 42))
        );
    }

    #[tokio::test]
    async fn canceled_receive_resumes_partial_header_and_payload() {
        for split in [7, BaseMessage::HEADER_SIZE + 3] {
            let mut frames = Vec::new();
            for id in [11, 12] {
                write_time(&mut frames, id, 0).await;
            }
            let (mut writer, mut reader) = tokio::io::duplex(frames.len());
            let mut decoder = FrameReader::default();
            writer.write_all(&frames[..split]).await.unwrap();
            tokio::select! {
                biased;
                result = decoder.read_frame(&mut reader) => panic!("incomplete frame: {result:?}"),
                () = std::future::ready(()) => {}
            }
            writer.write_all(&frames[split..]).await.unwrap();
            drop(writer);
            for id in [11, 12] {
                let msg = decoder.read_frame(&mut reader).await.unwrap();
                assert_eq!(msg.base.id, id);
                assert!(
                    matches!(msg.payload, MessagePayload::Time(t) if t.latency.usec == i32::from(id))
                );
            }
        }
    }

    #[test]
    fn rejects_websocket_audio_scheme() {
        assert!(SnapConnection::new("ws", "localhost", 1780).is_err());
        assert!(SnapConnection::new("wss", "localhost", 1788).is_err());
    }

    #[test]
    fn snapconnection_new_accepts_tcp() {
        let conn = SnapConnection::new(snapcast_proto::SCHEME_TCP, "localhost", 1704).unwrap();
        assert!(matches!(conn, SnapConnection::Tcp(_)));
    }

    #[test]
    fn snapconnection_new_rejects_unknown_scheme() {
        assert!(SnapConnection::new("gopher", "localhost", 70).is_err());
    }

    // ---- read_frame error paths ----

    #[tokio::test]
    async fn read_frame_empty_reader_errors() {
        // Header read_exact fails immediately on an empty reader.
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        assert!(read_frame(&mut cursor).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_truncated_header_errors() {
        // Fewer than HEADER_SIZE bytes → header read_exact fails.
        let mut cursor = std::io::Cursor::new(vec![0u8; BaseMessage::HEADER_SIZE - 1]);
        assert!(read_frame(&mut cursor).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_truncated_payload_errors() {
        // A valid frame with its last payload byte chopped off: the header parses,
        // but the payload read_exact runs short.
        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 5 },
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 1,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();
        buf.truncate(buf.len() - 1);
        let mut cursor = std::io::Cursor::new(buf);
        assert!(read_frame(&mut cursor).await.is_err());
    }

    // ---- TcpConnection "not connected" paths ----

    #[tokio::test]
    async fn send_when_not_connected_errors() {
        let mut conn = TcpConnection::new("localhost", 1704);
        let payload = MessagePayload::Time(Time::default());
        assert!(conn.send(MessageType::Time, &payload).await.is_err());
    }

    #[tokio::test]
    async fn recv_when_not_connected_errors() {
        let mut conn = TcpConnection::new("localhost", 1704);
        assert!(conn.recv().await.is_err());
    }

    #[tokio::test]
    async fn send_request_when_not_connected_errors_and_advances_id() {
        let mut conn = TcpConnection::new("localhost", 1704);
        conn.next_id = u16::MAX; // exercise the wrapping_add
        let payload = MessagePayload::Time(Time::default());
        let res = conn
            .send_request(MessageType::Time, &payload, Duration::from_millis(10))
            .await;
        assert!(res.is_err(), "not connected");
        assert_eq!(
            conn.next_id, 1,
            "request id 0 is reserved; wrap goes back to 1"
        );
    }

    async fn write_time(writer: &mut (impl AsyncWriteExt + Unpin), id: u16, refers_to: u16) {
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id,
            refers_to,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let payload = MessagePayload::Time(Time {
            latency: Timeval {
                sec: 0,
                usec: i32::from(id),
            },
        });
        write_frame(writer, &mut base, &payload).await.unwrap();
    }

    #[tokio::test]
    async fn request_matches_reply_and_preserves_other_messages_in_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();

            let request = read_frame(&mut sock).await.unwrap();
            assert_eq!(request.base.msg_type, MessageType::Time);
            assert_ne!(request.base.id, 0);
            write_time(&mut sock, 99, 0).await;
            write_time(&mut sock, 100, request.base.id.wrapping_add(1)).await;
            write_time(&mut sock, 101, request.base.id).await;
            write_time(&mut sock, 102, 0).await;
        });

        let mut conn = TcpConnection::new("127.0.0.1", addr.port());
        conn.connect().await.unwrap();
        let reply = conn
            .send_request(
                MessageType::Time,
                &MessagePayload::Time(Time::new()),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(reply.base.id, 101);
        for id in [99, 100, 102] {
            let msg = tokio::time::timeout(Duration::from_secs(2), conn.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(msg.base.id, id);
        }

        server.await.unwrap();
    }

    #[tokio::test]
    async fn timed_out_or_canceled_request_leaves_its_late_reply_readable() {
        for cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let mut conn = TcpConnection::new("127.0.0.1", addr.port());
            conn.connect().await.unwrap();
            let (mut peer, _) = listener.accept().await.unwrap();

            let payload = MessagePayload::Time(Time::default());
            let id = if cancel {
                tokio::select! {
                    request = conn.send_request(MessageType::Time, &payload, Duration::from_secs(2)) => {
                        panic!("request completed before cancellation: {request:?}");
                    }
                    observed = read_frame(&mut peer) => observed.unwrap().base.id,
                }
            } else {
                let err = conn
                    .send_request(MessageType::Time, &payload, Duration::from_millis(20))
                    .await
                    .unwrap_err();
                assert_eq!(err.to_string(), "request timed out");
                tokio::time::timeout(Duration::from_secs(2), read_frame(&mut peer))
                    .await
                    .unwrap()
                    .unwrap()
                    .base
                    .id
            };
            write_time(&mut peer, 99, id).await;
            let msg = tokio::time::timeout(Duration::from_secs(2), conn.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(msg.base.refers_to, id);
            assert_eq!(msg.base.id, 99);
        }
    }

    // ---- steady-clock helpers ----

    #[test]
    fn time_helpers_produce_positive_values() {
        assert!(now_usec() > 0);
        let tv = steady_time_of_day();
        assert!(tv.sec > 0 || tv.usec > 0);
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        stamp_sent(&mut base);
        assert!(base.sent.sec > 0 || base.sent.usec > 0);
    }
}
