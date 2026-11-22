//! Experimental WebSocket-over-TLS transport for binary Snapcast frames.
//!
//! Frame send/receive is shared with the plain-WS transport (see [`super::ws`]);
//! only connection establishment (the TLS handshake) differs here.

use std::sync::Arc;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use snapcast_proto::MessageType;
use snapcast_proto::message::factory::{MessagePayload, TypedMessage};
use tokio_tungstenite::Connector;

use super::ws::{WsStream, frame_config, recv_frame, send_frame};

/// TLS options for WSS transport.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WssConfig {
    /// Optional PEM file containing additional trusted CA certificates for server validation.
    pub server_certificate: Option<PathBuf>,
    /// Optional PEM file containing the client certificate chain for mutual TLS.
    pub certificate: Option<PathBuf>,
    /// Optional PEM file containing the unencrypted private key for client authentication.
    pub certificate_key: Option<PathBuf>,
    /// Optional password for encrypted private keys (currently unsupported).
    pub key_password: Option<String>,
}

/// Experimental TLS counterpart to [`super::ws::WsConnection`].
pub struct WssConnection {
    ws: Option<WsStream>,
    host: String,
    port: u16,
    config: WssConfig,
}

impl WssConnection {
    /// Create a new WSS connection descriptor.
    pub fn new(host: &str, port: u16, config: WssConfig) -> Self {
        Self {
            ws: None,
            host: host.to_string(),
            port,
            config,
        }
    }

    /// Establish the WSS connection.
    ///
    /// A successful upgrade does not imply the peer supports binary Snapcast frames.
    pub async fn connect(&mut self) -> Result<()> {
        let url = format!("wss://{}:{}/jsonrpc", self.host, self.port);

        let connector = Connector::Rustls(self.build_tls_config()?);

        let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
            &url,
            Some(frame_config()),
            false,
            Some(connector),
        )
        .await
        .with_context(|| format!("WSS connect to {url}"))?;

        self.ws = Some(ws);
        Ok(())
    }

    fn build_tls_config(&self) -> Result<Arc<ClientConfig>> {
        let mut root_store = rustls::RootCertStore::empty();
        let loader = rustls_native_certs::load_native_certs();
        if !loader.errors.is_empty() {
            tracing::warn!("errors loading some native certs: {:?}", loader.errors);
        }
        for cert in loader.certs {
            root_store.add(cert)?;
        }
        if let Some(path) = &self.config.server_certificate {
            let certs = load_certificates(path).with_context(|| {
                format!("failed to load server CA certificate from {}", path.display())
            })?;
            anyhow::ensure!(
                !certs.is_empty(),
                "no certificates found in {}",
                path.display()
            );
            for cert in certs {
                root_store.add(cert)?;
            }
        }

        let builder = ClientConfig::builder().with_root_certificates(root_store);
        let config = match (&self.config.certificate, &self.config.certificate_key) {
            (None, None) => builder.with_no_client_auth(),
            (Some(cert), Some(key)) => {
                if self.config.key_password.is_some() {
                    bail!("encrypted private keys are not supported for WSS client auth yet");
                }
                let cert_chain = load_certificates(cert).with_context(|| {
                    format!("failed to load client certificate chain from {}", cert.display())
                })?;
                anyhow::ensure!(
                    !cert_chain.is_empty(),
                    "no certificates found in {}",
                    cert.display()
                );
                let key_der = PrivateKeyDer::from_pem_file(key).with_context(|| {
                    format!("failed to load client private key from {}", key.display())
                })?;
                builder.with_client_auth_cert(cert_chain, key_der)?
            }
            _ => bail!("both --cert and --cert-key are required for WSS client authentication"),
        };
        Ok(Arc::new(config))
    }

    /// Close the WSS connection.
    pub fn disconnect(&mut self) {
        self.ws = None;
    }

    /// Send one binary Snapcast frame over WSS.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        send_frame(
            self.ws.as_mut().context("not connected")?,
            msg_type,
            payload,
        )
        .await
    }

    /// Receive one binary Snapcast frame over WSS.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        recv_frame(self.ws.as_mut().context("not connected")?).await
    }
}

fn load_certificates(path: &std::path::Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_proto::message::time::Time;
    use tokio::net::TcpListener;

    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn wss_connection_new_sets_target() {
        let conn = WssConnection::new("example.invalid", 1788, WssConfig::default());
        assert!(conn.ws.is_none());
        assert_eq!(conn.host, "example.invalid");
        assert_eq!(conn.port, 1788);
        assert_eq!(conn.config, WssConfig::default());
    }

    #[test]
    fn disconnect_is_idempotent_when_not_connected() {
        let mut conn = WssConnection::new("example.invalid", 1788, WssConfig::default());
        conn.disconnect();
        conn.disconnect();
        assert!(conn.ws.is_none());
    }

    #[tokio::test]
    async fn send_recv_when_not_connected_error() {
        let mut conn = WssConnection::new("example.invalid", 1788, WssConfig::default());
        let payload = MessagePayload::Time(Time::default());
        assert!(conn.send(MessageType::Time, &payload).await.is_err());
        assert!(conn.recv().await.is_err());
    }

    #[test]
    fn build_tls_config_rejects_password_protected_keys() {
        ensure_crypto_provider();
        let conn = WssConnection::new(
            "example.invalid",
            1788,
            WssConfig {
                certificate: Some("client.crt".into()),
                certificate_key: Some("client.key".into()),
                key_password: Some("secret".into()),
                ..WssConfig::default()
            },
        );
        let err = conn.build_tls_config().unwrap_err();
        assert!(err.to_string().contains("encrypted private keys"));
    }

    #[test]
    fn build_tls_config_requires_cert_and_key_together() {
        ensure_crypto_provider();
        let only_cert = WssConnection::new(
            "example.invalid",
            1788,
            WssConfig {
                certificate: Some("client.crt".into()),
                ..WssConfig::default()
            },
        );
        assert!(only_cert
            .build_tls_config()
            .unwrap_err()
            .to_string()
            .contains("both --cert and --cert-key are required"));
        let only_key = WssConnection::new(
            "example.invalid",
            1788,
            WssConfig {
                certificate_key: Some("client.key".into()),
                ..WssConfig::default()
            },
        );
        assert!(only_key
            .build_tls_config()
            .unwrap_err()
            .to_string()
            .contains("both --cert and --cert-key are required"));
    }

    #[test]
    fn build_tls_config_rejects_invalid_server_ca_path() {
        ensure_crypto_provider();
        let conn = WssConnection::new(
            "example.invalid",
            1788,
            WssConfig {
                server_certificate: Some("/no/such/path.pem".into()),
                ..WssConfig::default()
            },
        );
        let err = conn.build_tls_config().unwrap_err();
        assert!(err.to_string().contains("server CA certificate"));
    }

    #[tokio::test]
    async fn connect_failure_includes_wss_context() {
        ensure_crypto_provider();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // Free the port so connect fails predictably.

        let mut conn = WssConnection::new("127.0.0.1", port, WssConfig::default());
        let err = conn.connect().await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("WSS connect to wss://127.0.0.1:"));
        assert!(msg.contains("/jsonrpc"));
    }
}
