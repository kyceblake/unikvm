use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, Incoming, TransportConfig, VarInt};
use rustls::client::Resumption;
use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, version};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};
use unikvm_protocol::peer::{ALPN, READY};

use crate::discovery::{Discovery, DiscoveryError};
use crate::identity::{Fingerprint, Identity, TLS_NAME, TrustedPeer};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(8);
const MAX_PEERS: usize = 16;
const MAX_HANDSHAKES: usize = 8;

pub struct PeerEndpoint {
    endpoint: Endpoint,
    own: Fingerprint,
    discovery: bool,
    peers: Arc<BTreeMap<Fingerprint, Peer>>,
}

struct Peer {
    trusted: TrustedPeer,
    manual: Option<SocketAddr>,
    client: quinn::ClientConfig,
    incoming_slot: Semaphore,
    connection: watch::Sender<Option<Connection>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerEvent {
    Connected(Fingerprint),
    Disconnected(Fingerprint),
}

#[derive(Debug, Error)]
pub enum BindError {
    #[error("configure between 1 and {MAX_PEERS} trusted peers")]
    PeerCount,
    #[error("local and trusted peer certificates must differ")]
    SelfPairing,
    #[error("trusted peer {0} is configured more than once")]
    DuplicatePeer(Fingerprint),
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
    #[error("peer did not present an approved certificate")]
    Identity,
    #[error("incoming connection does not match the configured address or dial direction")]
    UnexpectedPeer,
    #[error("peer already has an active session")]
    Duplicate,
    #[error("invalid peer readiness frame")]
    Ready,
    #[error("could not send readiness frame: {0}")]
    Send(#[from] quinn::WriteError),
    #[error("could not finish readiness stream: {0}")]
    Finish(#[from] quinn::ClosedStream),
    #[error("could not receive readiness frame: {0}")]
    Receive(#[from] quinn::ReadToEndError),
}

impl PeerEndpoint {
    pub fn bind(
        local: SocketAddr,
        discovery: bool,
        identity: &Identity,
        approved: Vec<(TrustedPeer, Option<SocketAddr>)>,
    ) -> Result<Self, BindError> {
        if !(1..=MAX_PEERS).contains(&approved.len()) {
            return Err(BindError::PeerCount);
        }
        let own = identity.fingerprint();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let transport = transport_config();
        let mut roots = RootCertStore::empty();
        let mut peers = BTreeMap::new();
        for (trusted, manual) in approved {
            let pin = trusted.fingerprint;
            if pin == own {
                return Err(BindError::SelfPairing);
            }
            if peers.contains_key(&pin) {
                return Err(BindError::DuplicatePeer(pin));
            }

            // A dial to a specific peer must authenticate that peer's certificate.
            let mut peer_root = RootCertStore::empty();
            peer_root.add(trusted.certificate.clone())?;
            roots.roots.extend(peer_root.roots.iter().cloned());
            let (certs, key) = identity.tls_credentials();
            let mut client = rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_protocol_versions(&[&version::TLS13])?
                .with_root_certificates(peer_root)
                .with_client_auth_cert(certs, key)?;
            client.alpn_protocols = vec![ALPN.to_vec()];
            client.resumption = Resumption::disabled();
            let mut client =
                quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client)?));
            client.transport_config(transport.clone());

            peers.insert(
                pin,
                Peer {
                    trusted,
                    manual,
                    client,
                    incoming_slot: Semaphore::new(1),
                    connection: watch::channel(None).0,
                },
            );
        }

        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()?;
        let (certs, key) = identity.tls_credentials();
        let mut server = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)?;
        server.alpn_protocols = vec![ALPN.to_vec()];
        server.send_tls13_tickets = 0;
        let mut server =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server)?));
        server.transport_config(transport);
        server
            .max_incoming(MAX_HANDSHAKES)
            .incoming_buffer_size(16 * 1024)
            .incoming_buffer_size_total(64 * 1024);
        let endpoint = Endpoint::server(server, local)?;
        Ok(Self {
            endpoint,
            own,
            discovery,
            peers: Arc::new(peers),
        })
    }

    /// Subscribe to the selected peer's mutually authenticated, ready connection.
    pub fn connection(&self, peer: Fingerprint) -> Option<watch::Receiver<Option<Connection>>> {
        self.peers
            .get(&peer)
            .map(|peer| peer.connection.subscribe())
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.own
    }

    /// Each pair has one connection, initiated by the lower full fingerprint.
    pub async fn run_until(
        self,
        shutdown: impl Future<Output = ()>,
        mut on_event: impl FnMut(PeerEvent),
    ) {
        let discovery = self
            .discovery
            .then(|| {
                self.endpoint
                    .local_addr()
                    .map_err(DiscoveryError::from)
                    .and_then(|local| Discovery::start(local, self.own))
                    .inspect_err(|error| warn!(%error, "could not start mDNS discovery"))
                    .ok()
            })
            .flatten();
        let mut workers = JoinSet::new();
        let (events, mut received) = mpsc::channel(MAX_PEERS * 2);
        let handshakes = Arc::new(Semaphore::new(MAX_HANDSHAKES));
        let mut hints = BTreeMap::new();
        for (&pin, peer) in self.peers.iter() {
            let dialer = self.own < pin;
            info!(peer = %pin, manual_address = ?peer.manual, discovery = self.discovery, dialer, "peer transport ready");
            if dialer {
                let (addresses, receiver) = watch::channel(Vec::new());
                hints.insert(pin, addresses);
                workers.spawn(dial_peer(
                    self.endpoint.clone(),
                    self.peers.clone(),
                    pin,
                    receiver,
                    events.clone(),
                ));
            }
        }

        {
            let browse = async {
                if let Some(discovery) = &discovery {
                    discovery.browse(&hints).await;
                }
                std::future::pending::<()>().await;
            };
            tokio::pin!(browse, shutdown);
            loop {
                tokio::select! {
                    () = &mut shutdown => break,
                    () = &mut browse => break,
                    Some(event) = received.recv() => on_event(event),
                    Some(result) = workers.join_next(), if !workers.is_empty() => {
                        if let Err(error) = result {
                            warn!(%error, "peer worker stopped unexpectedly");
                        }
                    }
                    incoming = self.endpoint.accept() => {
                        let Some(incoming) = incoming else { break };
                        self.accept(incoming, &handshakes, &events, &mut workers);
                    }
                }
            }
        }

        self.endpoint.close(0u32.into(), b"agent stopping");
        workers.shutdown().await;
        for peer in self.peers.values() {
            peer.connection.send_replace(None);
        }
        let _ = timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
        if let Some(discovery) = discovery {
            discovery.shutdown().await;
        }
    }

    fn accept(
        &self,
        incoming: Incoming,
        handshakes: &Arc<Semaphore>,
        events: &mpsc::Sender<PeerEvent>,
        workers: &mut JoinSet<()>,
    ) {
        if !self.discovery
            && !self.peers.iter().any(|(&pin, peer)| {
                pin < self.own
                    && peer
                        .manual
                        .is_some_and(|address| address.ip() == incoming.remote_address().ip())
            })
        {
            incoming.refuse();
            return;
        }
        if !incoming.remote_address_validated() {
            let _ = incoming.retry();
            return;
        }
        let Ok(permit) = handshakes.clone().try_acquire_owned() else {
            incoming.refuse();
            return;
        };
        workers.spawn(receive_peer(
            incoming,
            self.peers.clone(),
            self.own,
            self.discovery,
            permit,
            events.clone(),
        ));
    }
}

fn transport_config() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(1))
        // Clipboard frames can share the connection with latency-sensitive input.
        .stream_receive_window(VarInt::from_u32(16 * 1024))
        .receive_window(VarInt::from_u32(32 * 1024))
        .send_window(128 * 1024)
        .datagram_receive_buffer_size(None)
        .datagram_send_buffer_size(0)
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .max_idle_timeout(Some(quinn::IdleTimeout::from(VarInt::from_u32(10_000))));
    Arc::new(transport)
}

async fn receive_peer(
    incoming: Incoming,
    peers: Arc<BTreeMap<Fingerprint, Peer>>,
    own: Fingerprint,
    discovery: bool,
    handshake_permit: OwnedSemaphorePermit,
    events: mpsc::Sender<PeerEvent>,
) {
    let result = timeout(CONNECT_TIMEOUT, async {
        let connection = incoming.await?;
        let certificate = presented_leaf(&connection)?;
        let pin = Fingerprint::for_certificate(&certificate);
        let peer = peers
            .get(&pin)
            .filter(|peer| peer.trusted.certificate == certificate)
            .ok_or(SessionError::Identity)?;
        if pin >= own
            || (!discovery
                && !peer
                    .manual
                    .is_some_and(|address| address.ip() == connection.remote_address().ip()))
        {
            return Err(SessionError::UnexpectedPeer);
        }
        let slot = peer
            .incoming_slot
            .try_acquire()
            .map_err(|_| SessionError::Duplicate)?;
        confirm(&connection).await?;
        Ok::<_, SessionError>((connection, pin, slot))
    })
    .await;
    drop(handshake_permit);
    match result {
        Ok(Ok((connection, pin, _slot))) => track(connection, pin, &peers[&pin], &events).await,
        Ok(Err(error)) => debug!(%error, "incoming peer connection rejected"),
        Err(_) => debug!("incoming peer connection timed out"),
    }
}

async fn dial_peer(
    endpoint: Endpoint,
    peers: Arc<BTreeMap<Fingerprint, Peer>>,
    pin: Fingerprint,
    mut hints: watch::Receiver<Vec<SocketAddr>>,
    events: mpsc::Sender<PeerEvent>,
) {
    let peer = &peers[&pin];
    let mut delay = Duration::from_millis(250);
    let mut next = 0usize;
    loop {
        let candidates = dial_candidates(peer.manual, &hints.borrow_and_update());
        if candidates.is_empty() {
            if hints.changed().await.is_err() {
                return;
            }
            continue;
        }
        let address = candidates[next % candidates.len()];
        next = next.wrapping_add(1);
        match timeout(CONNECT_TIMEOUT, dial(&endpoint, address, peer)).await {
            Ok(Ok(connection)) => {
                track(connection, pin, peer, &events).await;
                delay = Duration::from_millis(250);
                next = 0;
            }
            Ok(Err(error)) => debug!(peer = %pin, %address, %error, "peer connection failed"),
            Err(_) => {
                debug!(peer = %pin, %address, error = %SessionError::Timeout, "peer connection failed")
            }
        }
        tokio::select! {
            () = sleep(delay) => delay = (delay * 2).min(MAX_RETRY),
            Ok(()) = hints.changed() => {
                delay = Duration::from_millis(250);
                next = 0;
            }
        }
    }
}

async fn dial(
    endpoint: &Endpoint,
    address: SocketAddr,
    peer: &Peer,
) -> Result<Connection, SessionError> {
    let connection = endpoint
        .connect_with(peer.client.clone(), address, TLS_NAME)?
        .await?;
    if presented_leaf(&connection)? != peer.trusted.certificate {
        return Err(SessionError::Identity);
    }
    confirm(&connection).await?;
    Ok(connection)
}

fn presented_leaf(connection: &Connection) -> Result<CertificateDer<'static>, SessionError> {
    connection
        .peer_identity()
        .and_then(|identity| identity.downcast::<Vec<CertificateDer<'static>>>().ok())
        .filter(|certificates| certificates.len() == 1)
        .and_then(|mut certificates| certificates.pop())
        .ok_or(SessionError::Identity)
}

async fn confirm(connection: &Connection) -> Result<(), SessionError> {
    let mut send = connection.open_uni().await?;
    send.write_all(READY).await?;
    send.finish()?;
    let mut receive = connection.accept_uni().await?;
    if receive.read_to_end(READY.len()).await? != READY {
        return Err(SessionError::Ready);
    }
    Ok(())
}

async fn track(
    connection: Connection,
    pin: Fingerprint,
    peer: &Peer,
    events: &mpsc::Sender<PeerEvent>,
) {
    peer.connection.send_replace(Some(connection.clone()));
    if events.send(PeerEvent::Connected(pin)).await.is_err() {
        return;
    }
    let error = connection.closed().await;
    peer.connection.send_replace(None);
    debug!(peer = %pin, %error, "peer connection closed");
    let _ = events.send(PeerEvent::Disconnected(pin)).await;
}

fn dial_candidates(manual: Option<SocketAddr>, hints: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut addresses = Vec::with_capacity(hints.len() + 1);
    addresses.extend(manual);
    addresses.extend(
        hints
            .iter()
            .copied()
            .filter(|address| Some(*address) != manual),
    );
    addresses
}

#[cfg(test)]
mod tests;
