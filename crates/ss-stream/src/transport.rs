//! The connection abstraction the client runs on, and its
//! `tokio-tungstenite` implementation.
//!
//! [`MixStreamClient`](crate::MixStreamClient) is generic over a
//! [`Connector`] so tests can drive it with an in-memory fake; production
//! code uses [`TungsteniteConnector`].

use std::future::Future;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::ClientRequestBuilder;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::TransportError;

/// Reference `max_size=2**22`: the largest WebSocket message accepted.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 4 << 20;

/// What a [`Transport`] hands up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    /// A binary message: one MixStream frame (see [`crate::parse_frame_bytes`]).
    Binary(Bytes),
    /// A text message; DSM is not known to send any.
    Text(String),
    /// The peer closed the connection, with its close reason if it gave one.
    Closed(Option<String>),
}

/// One open WebSocket connection.
pub trait Transport: Send {
    /// Wait for the next message. **Must be cancel-safe**: the client
    /// drops this future whenever a command or timer wins its `select!`.
    fn recv(&mut self) -> impl Future<Output = Result<WsMessage, TransportError>> + Send;
    /// Send a text message (in-band command or keepalive).
    fn send_text(&mut self, text: &str) -> impl Future<Output = Result<(), TransportError>> + Send;
    /// Close the connection; errors are irrelevant to the caller.
    fn close(&mut self) -> impl Future<Output = ()> + Send;
}

/// Opens connections. `sid` is the DSM session id to authenticate with.
pub trait Connector: Send + Sync + 'static {
    /// The connection type.
    type Conn: Transport + 'static;
    /// Open `url`, authenticated as `sid`.
    fn connect(
        &self,
        url: &str,
        sid: &str,
    ) -> impl Future<Output = Result<Self::Conn, TransportError>> + Send;
}

/// The real thing: `tokio-tungstenite` over TCP or rustls.
///
/// Authentication is exactly the reference's (`ws_bridge.py` `_pump`):
/// a `Cookie: id=<sid>` header on the HTTP upgrade request, nothing in
/// the URL. Protocol pings are not sent (the reference sets
/// `ping_interval=None`); the client's application-level `keepAlive`
/// text message does that job.
#[derive(Debug, Clone)]
pub struct TungsteniteConnector {
    /// Skip certificate chain validation (self-signed NAS certificates).
    /// Signatures are still checked; the chain and hostname are not.
    pub accept_invalid_certs: bool,
    /// Largest message accepted before the connection is dropped.
    pub max_message_size: usize,
}

impl Default for TungsteniteConnector {
    fn default() -> Self {
        Self {
            accept_invalid_certs: false,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        }
    }
}

impl TungsteniteConnector {
    /// Validate certificates against the bundled webpki roots.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept any certificate (self-signed NAS).
    pub fn insecure() -> Self {
        Self {
            accept_invalid_certs: true,
            ..Self::default()
        }
    }
}

/// A live `tokio-tungstenite` connection.
pub struct TungsteniteTransport {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl std::fmt::Debug for TungsteniteTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TungsteniteTransport")
            .finish_non_exhaustive()
    }
}

fn map_connect_error(e: WsError) -> TransportError {
    match e {
        // Only the status: the response could echo request details.
        WsError::Http(resp) => {
            TransportError::Connect(format!("handshake failed: HTTP {}", resp.status()))
        }
        other => TransportError::Connect(other.to_string()),
    }
}

fn map_io_error(e: WsError) -> TransportError {
    match e {
        WsError::ConnectionClosed | WsError::AlreadyClosed => TransportError::Closed,
        other => TransportError::Io(other.to_string()),
    }
}

impl Connector for TungsteniteConnector {
    type Conn = TungsteniteTransport;

    async fn connect(&self, url: &str, sid: &str) -> Result<Self::Conn, TransportError> {
        let uri: Uri =
            url.parse()
                .map_err(|e: tokio_tungstenite::tungstenite::http::uri::InvalidUri| {
                    TransportError::Connect(format!("invalid URL: {e}"))
                })?;
        let request = ClientRequestBuilder::new(uri).with_header("Cookie", format!("id={sid}"));
        let config = WebSocketConfig::default()
            .max_message_size(Some(self.max_message_size))
            .max_frame_size(Some(self.max_message_size));
        let tls = self
            .accept_invalid_certs
            .then(|| tokio_tungstenite::Connector::Rustls(insecure_tls_config()));
        let (ws, _response) =
            tokio_tungstenite::connect_async_tls_with_config(request, Some(config), true, tls)
                .await
                .map_err(map_connect_error)?;
        Ok(TungsteniteTransport { ws })
    }
}

impl Transport for TungsteniteTransport {
    async fn recv(&mut self) -> Result<WsMessage, TransportError> {
        loop {
            match self.ws.next().await {
                None => return Ok(WsMessage::Closed(None)),
                Some(Err(e)) => return Err(map_io_error(e)),
                Some(Ok(Message::Binary(b))) => return Ok(WsMessage::Binary(b)),
                Some(Ok(Message::Text(t))) => return Ok(WsMessage::Text(t.to_string())),
                Some(Ok(Message::Close(frame))) => {
                    return Ok(WsMessage::Closed(
                        frame.map(|f| format!("{} {}", f.code, f.reason)),
                    ));
                }
                // Pings are answered by tungstenite itself; pongs and raw frames carry nothing.
                Some(Ok(_)) => continue,
            }
        }
    }

    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        self.ws
            .send(Message::text(text))
            .await
            .map_err(map_io_error)
    }

    async fn close(&mut self) {
        let _ = self.ws.close(None).await;
    }
}

/// A rustls config that accepts any server certificate but still verifies
/// handshake signatures. Built once, on the `ring` provider explicitly so
/// it never depends on which provider the rest of the process installed.
fn insecure_tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .expect("ring supports rustls' default protocol versions")
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert { provider }))
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

#[derive(Debug)]
struct AcceptAnyCert {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
