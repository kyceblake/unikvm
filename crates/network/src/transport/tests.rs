use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use quinn::Endpoint;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::{
    BindError, Fingerprint, Identity, PeerEndpoint, PeerEvent, TrustedPeer, dial_candidates,
};

const WAIT: Duration = Duration::from_secs(8);

fn approve(identity: &Identity) -> TrustedPeer {
    TrustedPeer::approve(identity.certificate().to_vec(), identity.fingerprint()).unwrap()
}

fn addresses<const N: usize>() -> [SocketAddr; N] {
    let held = [(); N].map(|()| UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap());
    held.each_ref().map(|socket| socket.local_addr().unwrap())
}

struct Running {
    endpoint: Endpoint,
    events: mpsc::Receiver<PeerEvent>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Running {
    fn start(peer: PeerEndpoint) -> Self {
        let endpoint = peer.endpoint.clone();
        let (stop, stopped) = oneshot::channel();
        let (events, received) = mpsc::channel(16);
        let task = tokio::spawn(peer.run_until(
            async {
                let _ = stopped.await;
            },
            move |event| {
                events.try_send(event).unwrap();
            },
        ));
        Self {
            endpoint,
            stop,
            task,
            events: received,
        }
    }

    async fn next(&mut self, expected: PeerEvent) {
        assert_eq!(
            timeout(WAIT, self.events.recv()).await.unwrap(),
            Some(expected)
        );
    }

    async fn two_connections(&mut self, first: Fingerprint, second: Fingerprint) {
        let seen = timeout(WAIT, async {
            [self.events.recv().await, self.events.recv().await]
        })
        .await
        .unwrap();
        assert!(seen.contains(&Some(PeerEvent::Connected(first))));
        assert!(seen.contains(&Some(PeerEvent::Connected(second))));
        assert_eq!(self.endpoint.open_connections(), 2);
    }

    async fn stop(self) {
        self.stop.send(()).unwrap();
        timeout(WAIT, self.task).await.unwrap().unwrap();
        // The test holds a Quinn handle for assertions. Wait for its UDP socket
        // to be released before simulating a restart at the same address.
        let address = self.endpoint.local_addr().unwrap();
        drop(self.endpoint);
        timeout(WAIT, async {
            loop {
                match UdpSocket::bind(address) {
                    Ok(_) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("could not rebind {address}: {error}"),
                }
            }
        })
        .await
        .expect("QUIC endpoint did not release its UDP socket");
    }
}

#[test]
fn prefers_the_manual_address_and_deduplicates_discovered_addresses() {
    let manual = "192.168.1.10:24800".parse().unwrap();
    let discovered = "192.168.1.20:24800".parse().unwrap();
    assert_eq!(
        dial_candidates(Some(manual), &[discovered, manual]),
        [manual, discovered]
    );
    assert_eq!(dial_candidates(None, &[discovered]), [discovered]);
    assert_eq!(dial_candidates(Some(manual), &[]), [manual]);
}

#[tokio::test]
async fn duplicate_and_self_pins_are_rejected_before_binding() {
    let [local] = addresses();
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    assert!(matches!(
        PeerEndpoint::bind(local, true, &a, vec![(approve(&b), None), (approve(&b), None)]),
        Err(BindError::DuplicatePeer(pin)) if pin == b.fingerprint()
    ));
    assert!(matches!(
        PeerEndpoint::bind(local, true, &a, vec![(approve(&a), None)]),
        Err(BindError::SelfPairing)
    ));
}

#[tokio::test]
async fn three_peers_stay_directly_connected_when_one_restarts() {
    let [a_addr, b_addr, c_addr] = addresses();
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let c = Identity::generate().unwrap();
    let node_b = |identity| {
        PeerEndpoint::bind(
            b_addr,
            false,
            identity,
            vec![(approve(&a), Some(a_addr)), (approve(&c), Some(c_addr))],
        )
        .unwrap()
    };
    let mut a_run = Running::start(
        PeerEndpoint::bind(
            a_addr,
            false,
            &a,
            vec![(approve(&b), Some(b_addr)), (approve(&c), Some(c_addr))],
        )
        .unwrap(),
    );
    let mut b_run = Running::start(node_b(&b));
    let mut c_run = Running::start(
        PeerEndpoint::bind(
            c_addr,
            false,
            &c,
            vec![(approve(&a), Some(a_addr)), (approve(&b), Some(b_addr))],
        )
        .unwrap(),
    );

    tokio::join!(
        a_run.two_connections(b.fingerprint(), c.fingerprint()),
        b_run.two_connections(a.fingerprint(), c.fingerprint()),
        c_run.two_connections(a.fingerprint(), b.fingerprint()),
    );

    b_run.stop().await;
    tokio::join!(
        a_run.next(PeerEvent::Disconnected(b.fingerprint())),
        c_run.next(PeerEvent::Disconnected(b.fingerprint())),
    );
    // Retrying B can briefly open an unauthenticated transport. The authenticated
    // peer event stream must stay quiet for A–C while B is offline.
    let offline_window = Duration::from_millis(600);
    let (a_event, c_event) = tokio::join!(
        timeout(offline_window, a_run.events.recv()),
        timeout(offline_window, c_run.events.recv()),
    );
    assert!(a_event.is_err());
    assert!(c_event.is_err());

    let restored = Identity::from_bytes(&b.to_bytes()).unwrap();
    let mut b_run = Running::start(node_b(&restored));
    tokio::join!(
        a_run.next(PeerEvent::Connected(b.fingerprint())),
        b_run.two_connections(a.fingerprint(), c.fingerprint()),
        c_run.next(PeerEvent::Connected(b.fingerprint())),
    );
    assert_eq!(a_run.endpoint.open_connections(), 2);
    assert_eq!(c_run.endpoint.open_connections(), 2);
    tokio::join!(a_run.stop(), b_run.stop(), c_run.stop());
}

#[tokio::test]
async fn rejects_unknown_client_and_unexpected_server_certificates() {
    let [listener_addr, stranger_addr, dialer_addr] = addresses();
    let listener = Identity::generate().unwrap();
    let stranger = Identity::generate().unwrap();
    let dialer = Identity::generate().unwrap();
    let other = Identity::generate().unwrap();
    let listener_identity = &listener;
    let listener = PeerEndpoint::bind(
        listener_addr,
        true,
        listener_identity,
        vec![(approve(&dialer), None), (approve(&other), None)],
    )
    .unwrap();
    let stranger = PeerEndpoint::bind(
        stranger_addr,
        false,
        &stranger,
        vec![(approve(listener_identity), Some(listener_addr))],
    )
    .unwrap();
    let dialer = PeerEndpoint::bind(
        dialer_addr,
        false,
        &dialer,
        vec![(approve(&other), Some(listener_addr))],
    )
    .unwrap();
    let listener_run = Running::start(listener);
    let cannot_dial = |endpoint: &PeerEndpoint| {
        let client = endpoint.peers.values().next().unwrap().client.clone();
        let remote = endpoint.endpoint.clone();
        async move {
            let attempt = remote
                .connect_with(client, listener_addr, super::TLS_NAME)
                .unwrap();
            let result = timeout(WAIT, async {
                if let Ok(connection) = attempt.await {
                    // A QUIC client can finish TLS before the server rejects its client cert.
                    return connection.accept_uni().await.is_err();
                }
                true
            })
            .await
            .unwrap();
            assert!(result);
        }
    };
    tokio::join!(cannot_dial(&stranger), cannot_dial(&dialer));
    listener_run.stop().await;
}

#[tokio::test]
async fn rejects_a_second_connection_from_an_authenticated_peer() {
    let [a_addr, b_addr] = addresses();
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let (dialer, listener) = if a.fingerprint() < b.fingerprint() {
        (a, b)
    } else {
        (b, a)
    };
    let a = PeerEndpoint::bind(
        a_addr,
        false,
        &dialer,
        vec![(approve(&listener), Some(b_addr))],
    )
    .unwrap();
    let b = PeerEndpoint::bind(
        b_addr,
        false,
        &listener,
        vec![(approve(&dialer), Some(a_addr))],
    )
    .unwrap();
    let client = a.peers[&listener.fingerprint()].client.clone();
    let mut a_run = Running::start(a);
    let mut b_run = Running::start(b);
    tokio::join!(
        a_run.next(PeerEvent::Connected(listener.fingerprint())),
        b_run.next(PeerEvent::Connected(dialer.fingerprint())),
    );

    let second = a_run
        .endpoint
        .connect_with(client, b_addr, super::TLS_NAME)
        .unwrap();
    let result = timeout(WAIT, async {
        if let Ok(connection) = second.await {
            assert!(connection.accept_uni().await.is_err());
        }
    })
    .await;
    assert!(result.is_ok());
    assert!(b_run.events.try_recv().is_err());
    // The rejected QUIC transport can remain registered while it drains.
    assert!(a_run.events.try_recv().is_err());
    tokio::join!(a_run.stop(), b_run.stop());
}

#[tokio::test]
async fn either_authenticated_endpoint_can_open_an_input_stream() {
    use tokio::io::AsyncWriteExt;
    use unikvm_core::input::{ButtonState, InputEvent, KeyUsage};
    use unikvm_protocol::input::{self, Frame, GRANT, REQUEST};

    let [a_addr, b_addr] = addresses();
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let a_endpoint =
        PeerEndpoint::bind(a_addr, false, &a, vec![(approve(&b), Some(b_addr))]).unwrap();
    let b_endpoint =
        PeerEndpoint::bind(b_addr, false, &b, vec![(approve(&a), Some(a_addr))]).unwrap();
    let mut a_connection = a_endpoint.connection(b.fingerprint()).unwrap();
    let mut b_connection = b_endpoint.connection(a.fingerprint()).unwrap();
    let mut a_run = Running::start(a_endpoint);
    let mut b_run = Running::start(b_endpoint);
    tokio::join!(
        a_run.next(PeerEvent::Connected(b.fingerprint())),
        b_run.next(PeerEvent::Connected(a.fingerprint())),
    );
    let a_connection = a_connection.borrow_and_update().clone().unwrap();
    let b_connection = b_connection.borrow_and_update().clone().unwrap();
    let frame = Frame::Event(InputEvent::Key {
        usage: KeyUsage(0xe1),
        state: ButtonState::Released,
    });

    for (sender, receiver) in [
        (&a_connection, &b_connection),
        (&b_connection, &a_connection),
    ] {
        timeout(WAIT, async {
            tokio::join!(
                async {
                    let (mut send, mut recv) = sender.open_bi().await.unwrap();
                    send.write_all(REQUEST).await.unwrap();
                    let mut grant = [0];
                    recv.read_exact(&mut grant).await.unwrap();
                    assert_eq!(grant, [GRANT]);
                    input::write(&mut send, frame).await.unwrap();
                    send.shutdown().await.unwrap();
                },
                async {
                    let (mut send, mut recv) = receiver.accept_bi().await.unwrap();
                    let mut request = [0; REQUEST.len()];
                    recv.read_exact(&mut request).await.unwrap();
                    assert_eq!(request, *REQUEST);
                    send.write_all(&[GRANT]).await.unwrap();
                    assert_eq!(input::read(&mut recv).await.unwrap(), frame);
                    assert_eq!(recv.read(&mut [0u8; 1]).await.unwrap(), None);
                },
            );
        })
        .await
        .unwrap();
    }
    drop((a_connection, b_connection));
    tokio::join!(a_run.stop(), b_run.stop());
}

#[tokio::test]
async fn clipboard_payload_larger_than_the_receive_window_can_cross_an_authenticated_link() {
    use std::num::NonZeroU64;
    use unikvm_core::clipboard::{Content, MAX_HTML_BYTES, MAX_TEXT_BYTES, PNG_SIGNATURE};
    use unikvm_protocol::clipboard::{self, READY, Revision, Update};

    let [a_addr, b_addr] = addresses();
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let a_endpoint =
        PeerEndpoint::bind(a_addr, false, &a, vec![(approve(&b), Some(b_addr))]).unwrap();
    let b_endpoint =
        PeerEndpoint::bind(b_addr, false, &b, vec![(approve(&a), Some(a_addr))]).unwrap();
    let mut a_link = a_endpoint.connection(b.fingerprint()).unwrap();
    let mut b_link = b_endpoint.connection(a.fingerprint()).unwrap();
    let mut a_run = Running::start(a_endpoint);
    let mut b_run = Running::start(b_endpoint);
    tokio::join!(
        a_run.next(PeerEvent::Connected(b.fingerprint())),
        b_run.next(PeerEvent::Connected(a.fingerprint())),
    );
    let a_link = a_link.borrow_and_update().clone().unwrap();
    let b_link = b_link.borrow_and_update().clone().unwrap();

    let exchange = |connection: quinn::Connection, origin, expected| async move {
        let mut send = connection.open_uni().await.unwrap();
        send.write_all(READY).await.unwrap();
        let mut recv = connection.accept_uni().await.unwrap();
        let mut ready = [0; READY.len()];
        recv.read_exact(&mut ready).await.unwrap();
        assert_eq!(&ready, READY);
        let text = "日本".repeat(MAX_TEXT_BYTES / 6);
        let revision = Revision {
            counter: NonZeroU64::MIN,
            origin,
        };
        let html = format!("<b>{}</b>", "あ".repeat((MAX_HTML_BYTES - 7) / 3));
        let update = Update::new(
            revision,
            Content::Text {
                plain: text,
                html: Some(html),
            },
        )
        .unwrap();
        let (sent, received) = tokio::join!(
            clipboard::write(&mut send, &update),
            clipboard::read(&mut recv),
        );
        sent.unwrap();
        let received = received.unwrap();
        assert_eq!(received.revision().origin, expected);
        assert!(matches!((update.content(), received.content()),
            (Content::Text { plain, html }, Content::Text { plain: received_plain, html: received_html })
            if plain == received_plain && html == received_html));
        // The transport treats PNG contents as opaque. Native adapters decode them.
        let mut png = PNG_SIGNATURE.to_vec();
        png.resize(512 * 1024, 21);
        let update = Update::new(revision, Content::Png(png)).unwrap();
        let (sent, received) = tokio::join!(
            clipboard::write(&mut send, &update),
            clipboard::read(&mut recv),
        );
        sent.unwrap();
        assert!(matches!((update.content(), received.unwrap().content()),
            (Content::Png(png), Content::Png(received_png)) if png == received_png));
    };
    let a_id = a.fingerprint().device_id();
    let b_id = b.fingerprint().device_id();
    timeout(WAIT, async {
        tokio::join!(exchange(a_link, a_id, b_id), exchange(b_link, b_id, a_id));
    })
    .await
    .unwrap();
    tokio::join!(a_run.stop(), b_run.stop());
}
