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
    let file = File::open(&config.peer_certificate).with_context(|| {
        format!(
            "open peer certificate {}",
            config.peer_certificate.display()
        )
    })?;
    let peer = TrustedPeer::read(file, config.peer_fingerprint.parse()?)?;
    PeerEndpoint::bind(
        config.listen,
        config.peer,
        config.discovery,
        &identity,
        peer,
    )
    .context("start authenticated peer endpoint")
}
