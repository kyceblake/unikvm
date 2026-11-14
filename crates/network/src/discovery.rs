use std::io;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::time::Duration;

use if_addrs::{IfAddr, Interface};
use mdns_sd::{
    IfKind, Receiver, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo,
};
use thiserror::Error;
use tokio::sync::watch;
use tokio::time::timeout;
use tracing::{debug, warn};
use unikvm_protocol::peer::{DISCOVERY_VERSION, SERVICE_TYPE};

use crate::identity::Fingerprint;

const VERSION_KEY: &str = "version";
const FINGERPRINT_KEY: &str = "fingerprint";
const MAX_CANDIDATES: usize = 4;

#[derive(Debug, Error)]
pub(crate) enum DiscoveryError {
    #[error("could not enumerate network interfaces")]
    Interfaces(#[from] io::Error),
    #[error("no active LAN interface has address {0}")]
    NoInterface(IpAddr),
    #[error("could not configure mDNS")]
    Mdns(#[from] mdns_sd::Error),
}

pub(crate) struct Discovery {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    interface: LanInterface,
    fullname: String,
    target: String,
}

impl Discovery {
    pub(crate) fn start(
        local: SocketAddr,
        own: Fingerprint,
        peer: Fingerprint,
    ) -> Result<Self, DiscoveryError> {
        let interface = LanInterface::for_address(local)?;
        let name = own.to_string();
        // The full fingerprint fits in a TXT record. A shorter hex label keeps DNS names valid.
        let mut service = ServiceInfo::new(
            SERVICE_TYPE,
            &name[..32],
            &format!("unikvm-{}.local.", &name[..32]),
            local.ip(),
            local.port(),
            &[
                (VERSION_KEY, DISCOVERY_VERSION),
                (FINGERPRINT_KEY, name.as_str()),
            ][..],
        )?;
        let selection = interface.selection();
        service.set_interfaces(vec![selection.clone()]);
        let daemon = ServiceDaemon::new()?;
        let configured = (|| {
            daemon.disable_interface(IfKind::All)?;
            daemon.enable_interface(selection)?;
            let fullname = service.get_fullname().to_owned();
            daemon.register(service)?;
            let events = daemon.browse(SERVICE_TYPE)?;
            Ok::<_, mdns_sd::Error>((events, fullname))
        })();
        let (events, fullname) = configured.inspect_err(|_| {
            let _ = daemon.shutdown();
        })?;

        let target = peer.to_string();
        let target = format!("{}.{SERVICE_TYPE}", &target[..32]);
        Ok(Self {
            daemon,
            events,
            interface,
            fullname,
            target,
        })
    }

    pub(crate) async fn browse(
        &self,
        expected: Fingerprint,
        addresses: watch::Sender<Vec<SocketAddr>>,
    ) {
        while let Ok(event) = self.events.recv_async().await {
            let candidates = match event {
                ServiceEvent::ServiceResolved(service)
                    if service.fullname.eq_ignore_ascii_case(&self.target) =>
                {
                    self.interface.candidates(&service, expected)
                }
                ServiceEvent::ServiceRemoved(_, name)
                    if name.eq_ignore_ascii_case(&self.target) =>
                {
                    Vec::new()
                }
                _ => continue,
            };
            addresses.send_if_modified(|current| {
                if *current == candidates {
                    return false;
                }
                *current = candidates;
                debug!(target_addresses = ?current, "updated mDNS address hints for configured peer");
                true
            });
        }
        addresses.send_replace(Vec::new());
        warn!("mDNS browser stopped");
    }

    pub(crate) async fn shutdown(self) {
        if let Ok(status) = self.daemon.unregister(&self.fullname) {
            let _ = timeout(Duration::from_millis(500), status.recv_async()).await;
        }
        if let Ok(status) = self.daemon.shutdown() {
            let _ = timeout(Duration::from_millis(500), status.recv_async()).await;
        }
    }
}

struct LanInterface {
    address: IfAddr,
    index: u32,
}

impl LanInterface {
    fn for_address(local: SocketAddr) -> Result<Self, DiscoveryError> {
        let interface = if_addrs::get_if_addrs()?.into_iter().find(|interface| {
            interface.ip() == local.ip()
                && interface.is_oper_up()
                && !interface.is_p2p()
                && !interface.is_loopback()
                && match local {
                    SocketAddr::V6(local) if local.scope_id() != 0 => {
                        interface.index == Some(local.scope_id())
                    }
                    _ => true,
                }
        });
        interface
            .and_then(Self::from_interface)
            .ok_or(DiscoveryError::NoInterface(local.ip()))
    }

    fn from_interface(interface: Interface) -> Option<Self> {
        Some(Self {
            address: interface.addr,
            index: interface.index.filter(|&index| index != 0)?,
        })
    }

    fn selection(&self) -> IfKind {
        match self.address {
            IfAddr::V4(_) => IfKind::IndexV4(self.index),
            IfAddr::V6(_) => IfKind::IndexV6(self.index),
        }
    }

    fn candidates(&self, service: &ResolvedService, expected: Fingerprint) -> Vec<SocketAddr> {
        if !service.ty_domain.eq_ignore_ascii_case(SERVICE_TYPE)
            || service.port == 0
            || service.get_property_val_str(VERSION_KEY) != Some(DISCOVERY_VERSION)
            || service
                .get_property_val_str(FINGERPRINT_KEY)
                .and_then(|value| value.parse().ok())
                != Some(expected)
        {
            return Vec::new();
        }
        let mut addresses: Vec<_> = service
            .addresses
            .iter()
            .filter_map(|address| self.candidate(address, service.port))
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        addresses.truncate(MAX_CANDIDATES);
        addresses
    }

    fn candidate(&self, address: &ScopedIp, port: u16) -> Option<SocketAddr> {
        // Accept hints from the configured interface on its directly connected subnet.
        match (&self.address, address) {
            (IfAddr::V4(local), ScopedIp::V4(peer)) => {
                let ip = *peer.addr();
                let mask = u32::from(local.netmask);
                (!ip.is_unspecified()
                    && !ip.is_multicast()
                    && !ip.is_loopback()
                    && ip != std::net::Ipv4Addr::BROADCAST
                    && Some(ip) != local.broadcast
                    && (local.prefixlen >= 31 || u32::from(ip) & !mask != 0)
                    && u32::from(ip) & mask == u32::from(local.ip) & mask
                    && peer
                        .interface_ids()
                        .iter()
                        .any(|interface| interface.index == self.index))
                .then(|| SocketAddr::from((ip, port)))
            }
            (IfAddr::V6(local), ScopedIp::V6(peer)) => {
                let ip = *peer.addr();
                let mask = u128::from(local.netmask);
                let scope = if ip.is_unicast_link_local() {
                    self.index
                } else {
                    0
                };
                (!ip.is_unspecified()
                    && !ip.is_multicast()
                    && !ip.is_loopback()
                    && ip.to_ipv4_mapped().is_none()
                    && u128::from(ip) & mask == u128::from(local.ip) & mask
                    && peer.scope_id().index == self.index)
                    .then(|| SocketAddrV6::new(ip, port, 0, scope).into())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use if_addrs::{IfAddr, Ifv4Addr};
    use mdns_sd::{InterfaceId, ScopedIp, ScopedIpV4, ServiceInfo};
    use unikvm_protocol::peer::{DISCOVERY_VERSION, SERVICE_TYPE};

    use super::{FINGERPRINT_KEY, LanInterface, VERSION_KEY};
    use crate::identity::Fingerprint;

    #[test]
    fn uses_only_matching_on_link_address_hints_for_the_configured_pin() {
        let pin: Fingerprint = "12".repeat(32).parse().unwrap();
        let wrong_pin: Fingerprint = "34".repeat(32).parse().unwrap();
        let local = LanInterface {
            index: 3,
            address: IfAddr::V4(Ifv4Addr {
                ip: Ipv4Addr::new(192, 168, 10, 7),
                netmask: Ipv4Addr::new(255, 255, 255, 0),
                prefixlen: 24,
                broadcast: Some(Ipv4Addr::new(192, 168, 10, 255)),
            }),
        };
        let mut service = ServiceInfo::new(
            SERVICE_TYPE,
            "test-peer",
            "test-peer.local.",
            "192.168.10.8",
            24800,
            &[
                (VERSION_KEY, DISCOVERY_VERSION),
                (FINGERPRINT_KEY, &pin.to_string()),
            ][..],
        )
        .unwrap()
        .as_resolved_service();
        service.addresses = [
            (Ipv4Addr::new(192, 168, 10, 8), 3),
            (Ipv4Addr::new(192, 168, 10, 9), 4),
            (Ipv4Addr::new(192, 168, 11, 8), 3),
            (Ipv4Addr::new(192, 168, 10, 255), 3),
            (Ipv4Addr::new(192, 168, 10, 0), 3),
        ]
        .into_iter()
        .map(|(address, index)| {
            ScopedIp::V4(ScopedIpV4::new(
                address,
                InterfaceId {
                    index,
                    name: String::new(),
                },
            ))
        })
        .collect();
        service
            .addresses
            .insert(ScopedIp::from("fe80::1".parse::<IpAddr>().unwrap()));
        assert_eq!(
            local.candidates(&service, pin),
            [SocketAddr::from(([192, 168, 10, 8], 24800))]
        );
        assert!(local.candidates(&service, wrong_pin).is_empty());
        service.port = 0;
        assert!(local.candidates(&service, pin).is_empty());
    }

    // Run explicitly on a host attached to a multicast-capable LAN.
    #[tokio::test]
    #[ignore = "requires a configured multicast LAN interface"]
    async fn dns_sd_connects_pinned_peers_without_manual_addresses() {
        use std::time::Duration;

        use tokio::sync::{mpsc, oneshot};
        use tokio::time::timeout;

        use crate::identity::{Identity, TrustedPeer};
        use crate::{PeerEndpoint, PeerEvent};

        let address: IpAddr = std::env::var("UNIKVM_TEST_LAN_IP")
            .unwrap()
            .parse()
            .unwrap();
        let local = SocketAddr::new(address, 0);
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let approve = |identity: &Identity| {
            TrustedPeer::approve(identity.certificate().to_vec(), identity.fingerprint()).unwrap()
        };
        let endpoint_a = PeerEndpoint::bind(local, None, true, &a, approve(&b)).unwrap();
        let endpoint_b = PeerEndpoint::bind(local, None, true, &b, approve(&a)).unwrap();
        let (events, mut received) = mpsc::channel(4);
        let (stop_a, stopped_a) = oneshot::channel::<()>();
        let (stop_b, stopped_b) = oneshot::channel::<()>();
        let other_events = events.clone();
        let task_a = tokio::spawn(endpoint_a.run_until(
            async {
                let _ = stopped_a.await;
            },
            move |event| {
                let _ = events.try_send(event);
            },
        ));
        let task_b = tokio::spawn(endpoint_b.run_until(
            async {
                let _ = stopped_b.await;
            },
            move |event| {
                let _ = other_events.try_send(event);
            },
        ));
        let verified = timeout(Duration::from_secs(15), async {
            let first = received.recv().await;
            let second = received.recv().await;
            assert_ne!(first, second);
            for fingerprint in [a.fingerprint(), b.fingerprint()] {
                assert!([first, second].contains(&Some(PeerEvent::Connected(fingerprint))));
            }
        })
        .await;
        let _ = stop_a.send(());
        let _ = stop_b.send(());
        task_a.await.unwrap();
        task_b.await.unwrap();
        verified.expect("mDNS did not connect both pinned endpoints");
    }
}
