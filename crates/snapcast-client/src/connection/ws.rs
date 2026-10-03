//! WebSocket connection to a snapserver.
//!
//! Hosts the frame send/receive shared with the WSS (TLS) transport — both use
//! tungstenite's `MaybeTlsStream`, so only connection establishment differs.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Shared WebSocket transport stream type.
///
/// tungstenite's `MaybeTlsStream` wraps both plain and TLS sockets, so the
/// plain-WS and WSS transports hold the same stream type and share frame I/O.
pub(super) type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Send one binary Snapcast frame over a (plain or TLS) WebSocket stream.
pub(super) async fn send_frame(
    ws: &mut WsStream,
    msg_type: MessageType,
    payload: &MessagePayload,
) -> Result<()> {
    let mut base = BaseMessage {
        msg_type,
        id: 0,
        refers_to: 0,
        sent: Timeval::default(),
        received: Timeval::default(),
        size: 0,
    };
    super::stamp_sent(&mut base);
    let frame =
        factory::serialize(&mut base, payload).map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
    ws.send(Message::Binary(frame.into())).await?;
    Ok(())
}

/// Receive one binary Snapcast frame from a (plain or TLS) WebSocket stream.
pub(super) async fn recv_frame(ws: &mut WsStream) -> Result<TypedMessage> {
    loop {
        let msg = ws
            .next()
            .await
            .context("WebSocket stream ended")?
            .context("WebSocket error")?;
        match msg {
            Message::Binary(data) => {
                if data.len() < BaseMessage::HEADER_SIZE {
                    continue;
                }
                let mut base = BaseMessage::read_from(&mut &data[..BaseMessage::HEADER_SIZE])
                    .map_err(|e| anyhow::anyhow!("parse header: {e}"))?;
                base.received = super::steady_time_of_day();
                super::ensure_payload_size(base.size)?;
                let payload = &data[BaseMessage::HEADER_SIZE..];
                anyhow::ensure!(
                    payload.len() == base.size as usize,
                    "payload size mismatch: header={}, actual={}",
                    base.size,
                    payload.len()
                );
                return factory::deserialize(base, payload)
                    .map_err(|e| anyhow::anyhow!("deserialize: {e}"));
            }
            Message::Close(_) => anyhow::bail!("WebSocket closed"),
            _ => continue, // skip text/ping/pong
        }
    }
}

/// WebSocket transport for Snapcast binary frames.
pub struct WsConnection {
    ws: Option<WsStream>,
    host: String,
    port: u16,
}

impl WsConnection {
    /// Create a new WebSocket connection descriptor.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            ws: None,
            host: host.to_string(),
            port,
        }
    }

    /// Establish the WebSocket connection.
    pub async fn connect(&mut self) -> Result<()> {
        let url = format!("ws://{}:{}/jsonrpc", self.host, self.port);
        let (ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("WebSocket connect to {url}"))?;
        self.ws = Some(ws);
        Ok(())
    }

    /// Close the WebSocket connection.
    pub fn disconnect(&mut self) {
        self.ws = None;
    }

    /// Send one binary Snapcast frame.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        send_frame(
            self.ws.as_mut().context("not connected")?,
            msg_type,
            payload,
        )
        .await
    }

    /// Receive one binary Snapcast frame.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        recv_frame(self.ws.as_mut().context("not connected")?).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_proto::message::time::Time;
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, connect_async};

    async fn ws_pair() -> Result<(WsStream, tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>)>
    {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await?;
            accept_async(sock).await.map_err(anyhow::Error::from)
        });
        let (client_ws, _) = connect_async(format!("ws://{}/jsonrpc", addr)).await?;
        let server_ws = server.await??;
        Ok((client_ws, server_ws))
    }

    #[tokio::test]
    async fn ws_connection_send_recv_when_not_connected_error() {
        let mut conn = WsConnection::new("localhost", 1704);
        let payload = MessagePayload::Time(Time::default());
        assert!(conn.send(MessageType::Time, &payload).await.is_err());
        assert!(conn.recv().await.is_err());
    }

    #[tokio::test]
    async fn recv_frame_skips_non_binary_and_short_binary() {
        let (mut client_ws, mut server_ws) = ws_pair().await.unwrap();
        server_ws.send(Message::Text("ignore".into())).await.unwrap();
        server_ws.send(Message::Binary(vec![1, 2, 3].into())).await.unwrap();

        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 77 },
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 11,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let frame = factory::serialize(&mut base, &payload).unwrap();
        server_ws.send(Message::Binary(frame.into())).await.unwrap();

        let msg = recv_frame(&mut client_ws).await.unwrap();
        assert_eq!(msg.base.msg_type, MessageType::Time);
        assert_eq!(msg.base.id, 11);
        match msg.payload {
            MessagePayload::Time(t) => assert_eq!(t.latency.usec, 77),
            _ => panic!("expected Time payload"),
        }
    }

    #[tokio::test]
    async fn recv_frame_payload_size_mismatch_errors() {
        let (mut client_ws, mut server_ws) = ws_pair().await.unwrap();
        let bad_header = BaseMessage {
            msg_type: MessageType::Time,
            id: 1,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 8,
        };
        let mut frame = bad_header.to_bytes().unwrap();
        frame.extend_from_slice(&[0, 0, 0, 0]);
        server_ws.send(Message::Binary(frame.into())).await.unwrap();

        let err = recv_frame(&mut client_ws).await.unwrap_err();
        assert!(err.to_string().contains("payload size mismatch"));
    }

    #[tokio::test]
    async fn recv_frame_close_errors() {
        let (mut client_ws, mut server_ws) = ws_pair().await.unwrap();
        server_ws.close(None).await.unwrap();

        let err = recv_frame(&mut client_ws).await.unwrap_err();
        assert!(err.to_string().contains("WebSocket closed"));
    }
}
