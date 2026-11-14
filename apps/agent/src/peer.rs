use std::fs::File;

use anyhow::{Context, Result};
use unikvm_config::NetworkConfig;
use unikvm_network::PeerEndpoint;
use unikvm_network::identity::{Identity, MAX_IDENTITY_SIZE, TrustedPeer};
use unikvm_platform::credential;

pub fn bind(config: &NetworkConfig) -> Result<PeerEndpoint> {
    let bytes = credential::read(&config.identity, MAX_IDENTITY_SIZE)
        .with_context(|| format!("read local identity {}", config.identity.display()))?;
    let identity = Identity::from_bytes(&bytes)?;
    let peers = config
        .peers
        .iter()
        .map(|peer| {
            let file = File::open(&peer.certificate)
                .with_context(|| format!("open peer certificate {}", peer.certificate.display()))?;
            let approved =
                TrustedPeer::read(file, peer.fingerprint.parse()?).with_context(|| {
                    format!("verify peer certificate {}", peer.certificate.display())
                })?;
            Ok((approved, peer.address))
        })
        .collect::<Result<Vec<_>>>()?;
    PeerEndpoint::bind(config.listen, config.discovery, &identity, peers)
        .context("start authenticated peer endpoint")
}
