use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, TransportConfig, VarInt};
use rustls::client::Resumption;
use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, version};
use thiserror::Error;
use tokio::time::{sleep, timeout};
use tracing::{debug, info};
use unikvm_protocol::peer::{ALPN, READY};

use crate::identity::{Fingerprint, Identity, TLS_NAME, TrustedPeer};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(8);

pub struct PeerEndpoint {
    endpoint: Endpoint,
    client: quinn::ClientConfig,
    address: SocketAddr,
    peer: TrustedPeer,
    dialer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerEvent {
    Connected(Fingerprint),
    Disconnected(Fingerprint),
}

#[derive(Debug, Error)]
pub enum BindError {
    #[error("local and trusted peer certificates must differ")]
    SelfPairing,
    #[error("invalid TLS identity or trusted certificate")]
    Tls(#[from] rustls::Error),
    #[error("could not build client certificate verifier")]
    Verifier(#[from] rustls::server::VerifierBuilderError),
    #[error("TLS provider has no QUIC initial cipher suite")]
    Quic(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
    #[error("could not open UDP listener")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
enum SessionError {
    #[error("connection attempt timed out")]
    Timeout,
    #[error("connection was rejected: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("could not start connection: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("peer did not present the approved certificate")]
    Identity,
    #[error("invalid peer readiness frame")]
    Ready,
    #[error("could not send readiness frame: {0}")]
    Send(#[from] quinn::WriteError),
    #[error("could not finish readiness stream: {0}")]
    Finish(#[from] quinn::ClosedStream),
    #[error("could not receive readiness frame: {0}")]
    Receive(#[from] quinn::ReadToEndError),
    #[error("endpoint is closed")]
    Closed,
}

impl PeerEndpoint {
    pub fn bind(
        local: SocketAddr,
        address: SocketAddr,
        identity: &Identity,
        peer: TrustedPeer,
    ) -> Result<Self, BindError> {
        let own = identity.fingerprint();
        if own == peer.fingerprint {
            return Err(BindError::SelfPairing);
        }

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        roots.add(peer.certificate.clone())?;
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots.clone()), provider.clone())
                .build()?;

        let (certs, key) = identity.tls_credentials();
        let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)?;
        server.alpn_protocols = vec![ALPN.to_vec()];
        server.send_tls13_tickets = 0;

        let (certs, key) = identity.tls_credentials();
        let mut client = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&version::TLS13])?
            .with_root_certificates(roots)
            .with_client_auth_cert(certs, key)?;
        client.alpn_protocols = vec![ALPN.to_vec()];
        client.resumption = Resumption::disabled();

        let mut transport = TransportConfig::default();
        transport
            .max_concurrent_bidi_streams(VarInt::from_u32(0))
            .max_concurrent_uni_streams(VarInt::from_u32(1))
            .stream_receive_window(VarInt::from_u32(256))
            .receive_window(VarInt::from_u32(256))
            .datagram_receive_buffer_size(None)
            .datagram_send_buffer_size(0)
            .keep_alive_interval(Some(Duration::from_secs(3)))
            .max_idle_timeout(Some(quinn::IdleTimeout::from(VarInt::from_u32(10_000))));
        let transport = Arc::new(transport);
        let mut server =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server)?));
        server.transport_config(transport.clone());
        let mut client = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client)?));
        client.transport_config(transport);

        server
            .max_incoming(8)
            .incoming_buffer_size(16 * 1024)
            .incoming_buffer_size_total(64 * 1024);
        let endpoint = Endpoint::server(server, local)?;
        Ok(Self {
            endpoint,
            client,
            address,
            dialer: own < peer.fingerprint,
            peer,
        })
    }

    #[cfg(test)]
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Both endpoints can listen and dial. The lower full fingerprint initiates this pair.
    pub async fn run_until(
        self,
        shutdown: impl Future<Output = ()>,
        mut on_event: impl FnMut(PeerEvent),
    ) {
        tokio::select! {
            () = self.maintain(&mut on_event) => {}
            () = shutdown => {}
        }
        self.endpoint.close(0u32.into(), b"agent stopping");
        let _ = timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }

    async fn maintain(&self, on_event: &mut impl FnMut(PeerEvent)) {
        info!(peer = %self.peer.fingerprint, address = %self.address, dialer = self.dialer, "peer transport ready");
        let mut delay = Duration::from_millis(250);
        loop {
            let result = if self.dialer {
                self.dial().await
            } else {
                self.receive().await
            };
            match result {
                Ok(connection) => {
                    on_event(PeerEvent::Connected(self.peer.fingerprint));
                    let error = connection.closed().await;
                    debug!(peer = %self.peer.fingerprint, %error, "peer connection closed");
                    on_event(PeerEvent::Disconnected(self.peer.fingerprint));
                    delay = Duration::from_millis(250);
                }
                Err(SessionError::Closed) => return,
                Err(error) => {
                    debug!(peer = %self.peer.fingerprint, %error, "peer connection failed")
                }
            }
            if self.dialer {
                sleep(delay).await;
                delay = (delay * 2).min(MAX_RETRY);
            }
        }
    }

    async fn dial(&self) -> Result<Connection, SessionError> {
        timeout(CONNECT_TIMEOUT, async {
            let connection = self
                .endpoint
                .connect_with(self.client.clone(), self.address, TLS_NAME)?
                .await?;
            self.confirm(connection).await
        })
        .await
        .map_err(|_| SessionError::Timeout)?
    }

    async fn receive(&self) -> Result<Connection, SessionError> {
        let incoming = loop {
            let incoming = self.endpoint.accept().await.ok_or(SessionError::Closed)?;
            if incoming.remote_address().ip() != self.address.ip() {
                incoming.refuse();
                continue;
            }
            if !incoming.remote_address_validated() {
                let _ = incoming.retry();
                continue;
            }
            break incoming;
        };
        timeout(CONNECT_TIMEOUT, async {
            let connection = incoming.await?;
            self.confirm(connection).await
        })
        .await
        .map_err(|_| SessionError::Timeout)?
    }

    async fn confirm(&self, connection: Connection) -> Result<Connection, SessionError> {
        let leaf = connection
            .peer_identity()
            .and_then(|identity| identity.downcast::<Vec<CertificateDer<'static>>>().ok())
            .filter(|certificates| {
                certificates.len() == 1 && certificates[0] == self.peer.certificate
            });
        if leaf.is_none() {
            connection.close(1u32.into(), b"peer identity mismatch");
            return Err(SessionError::Identity);
        }

        let mut send = connection.open_uni().await?;
        send.write_all(READY).await?;
        send.finish()?;
        let mut receive = connection.accept_uni().await?;
        if receive.read_to_end(READY.len()).await? != READY {
            connection.close(1u32.into(), b"invalid peer readiness");
            return Err(SessionError::Ready);
        }
        Ok(connection)
    }
}

#[cfg(test)]
mod tests;
