use std::net::SocketAddr;
use std::time::Duration;

use tokio::time::timeout;

use super::{Identity, PeerEndpoint, SessionError, TLS_NAME, TrustedPeer, dial_candidates};

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

fn pair() -> (PeerEndpoint, PeerEndpoint, Identity, Identity) {
    let local = SocketAddr::from(([127, 0, 0, 1], 0));
    let a_id = Identity::generate().unwrap();
    let b_id = Identity::generate().unwrap();
    let approved_a =
        || TrustedPeer::approve(a_id.certificate().to_vec(), a_id.fingerprint()).unwrap();
    let approved_b =
        || TrustedPeer::approve(b_id.certificate().to_vec(), b_id.fingerprint()).unwrap();
    // Allocate B first, then set its dial target to A once A has a bound port.
    let mut b = PeerEndpoint::bind(local, Some(local), false, &b_id, approved_a()).unwrap();
    let a = PeerEndpoint::bind(
        local,
        Some(b.local_addr().unwrap()),
        false,
        &a_id,
        approved_b(),
    )
    .unwrap();
    b.address = Some(a.local_addr().unwrap());
    (a, b, a_id, b_id)
}

#[tokio::test]
async fn mutually_pinned_peers_can_connect_in_both_directions() {
    let (a, b, _, _) = pair();
    let (outgoing, incoming) = tokio::join!(a.dial(b.local_addr().unwrap()), b.receive());
    let (a_conn, b_conn) = (outgoing.unwrap(), incoming.unwrap());
    a_conn.close(0u32.into(), b"test rotation");
    timeout(Duration::from_secs(2), b_conn.closed())
        .await
        .unwrap();

    let (outgoing, incoming) = tokio::join!(b.dial(a.local_addr().unwrap()), a.receive());
    assert!(outgoing.is_ok());
    assert!(incoming.is_ok());
}

#[tokio::test]
async fn running_peer_reconnects_when_listener_restarts() {
    use tokio::sync::{mpsc, oneshot};

    use super::PeerEvent;

    let (a, b, a_id, b_id) = pair();
    let (dialer, listener, dialer_id, listener_id) = if a.dialer {
        (a, b, a_id, b_id)
    } else {
        (b, a, b_id, a_id)
    };
    let listen_address = listener.local_addr().unwrap();
    let dial_address = dialer.local_addr().unwrap();
    let pin = listener_id.fingerprint();
    let (events, mut seen) = mpsc::channel(4);
    let dial_task = tokio::spawn(dialer.run_until(std::future::pending(), move |event| {
        events.try_send(event).unwrap();
    }));
    let (stop, stopped) = oneshot::channel::<()>();
    let listen_task = tokio::spawn(listener.run_until(
        async {
            let _ = stopped.await;
        },
        |_| {},
    ));

    assert_eq!(
        timeout(Duration::from_secs(6), seen.recv()).await.unwrap(),
        Some(PeerEvent::Connected(pin))
    );
    stop.send(()).unwrap();
    listen_task.await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(6), seen.recv()).await.unwrap(),
        Some(PeerEvent::Disconnected(pin))
    );

    let restored = Identity::from_bytes(&listener_id.to_bytes()).unwrap();
    let listener = PeerEndpoint::bind(
        listen_address,
        Some(dial_address),
        false,
        &restored,
        TrustedPeer::approve(dialer_id.certificate().to_vec(), dialer_id.fingerprint()).unwrap(),
    )
    .unwrap();
    let listener_task = tokio::spawn(listener.run_until(std::future::pending(), |_| {}));
    assert_eq!(
        timeout(Duration::from_secs(6), seen.recv()).await.unwrap(),
        Some(PeerEvent::Connected(pin))
    );
    dial_task.abort();
    listener_task.abort();
}

#[tokio::test]
async fn rejects_unknown_client_and_server_certificates() {
    let (a, b, a_id, b_id) = pair();
    let stranger = Identity::generate().unwrap();
    let local = SocketAddr::from(([127, 0, 0, 1], 0));
    let unapproved = PeerEndpoint::bind(
        local,
        Some(b.local_addr().unwrap()),
        false,
        &stranger,
        TrustedPeer::approve(b_id.certificate().to_vec(), b_id.fingerprint()).unwrap(),
    )
    .unwrap();

    let (client_result, server_result) =
        tokio::join!(unapproved.dial(b.local_addr().unwrap()), b.receive());
    assert!(matches!(client_result, Err(SessionError::Connection(_))));
    assert!(matches!(server_result, Err(SessionError::Connection(_))));

    let unknown_server = PeerEndpoint::bind(
        local,
        Some(a.local_addr().unwrap()),
        false,
        &stranger,
        TrustedPeer::approve(a_id.certificate().to_vec(), a_id.fingerprint()).unwrap(),
    )
    .unwrap();
    let (client_result, server_result) = tokio::join!(
        async {
            a.endpoint
                .connect_with(
                    a.client.clone(),
                    unknown_server.local_addr().unwrap(),
                    TLS_NAME,
                )
                .unwrap()
                .await
        },
        unknown_server.receive(),
    );
    assert!(client_result.is_err());
    assert!(matches!(server_result, Err(SessionError::Connection(_))));
}
