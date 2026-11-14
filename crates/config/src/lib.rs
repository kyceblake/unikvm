//! Configuration for the local agent.

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

const SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    schema_version: u16,
    #[serde(default)]
    pub log_level: LogLevel,
    pub network: Option<NetworkConfig>,
    pub input: Option<InputConfig>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    pub mode: InputMode,
    #[serde(default)]
    pub allow_locked_receive: bool,
    pub peer_fingerprint: String,
    pub peer_side: Option<PeerSide>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputMode {
    Send,
    Receive,
    Bidirectional,
}

/// Explicit local UDP bind and manually approved peers.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub listen: SocketAddr,
    #[serde(default)]
    pub discovery: bool,
    pub identity: PathBuf,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    peer: Option<SocketAddr>,
    peer_certificate: Option<PathBuf>,
    peer_fingerprint: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    pub address: Option<SocketAddr>,
    pub certificate: PathBuf,
    pub fingerprint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            log_level: LogLevel::default(),
            network: None,
            input: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read configuration")]
    Read(#[from] io::Error),
    #[error("invalid TOML configuration")]
    Toml(#[from] toml::de::Error),
    #[error("unsupported configuration schema version {0}")]
    Version(u16),
    #[error(
        "listen and peer must have matching address families and nonzero ports, and peer must be a unicast address"
    )]
    NetworkAddress,
    #[error("configure a manual peer address or enable discovery")]
    PeerAddressRequired,
    #[error("configure at least one trusted peer")]
    NoPeers,
    #[error(
        "the single-peer format requires both peer_certificate and peer_fingerprint and cannot be mixed with [[network.peers]]"
    )]
    LegacyPeer,
    #[error("discovery requires listen to use a specific non-loopback LAN address")]
    DiscoveryAddress,
    #[error("input.peer_fingerprint must identify a peer already trusted in [network]")]
    InputPeer,
    #[error("input.peer_side requires bidirectional input mode")]
    InputLayout,
    #[error("input.allow_locked_receive requires receive or bidirectional input mode")]
    LockedReceiveMode,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut config = Self::parse(&fs::read_to_string(path)?)?;
        if let Some(network) = &mut config.network {
            let directory = path.parent().unwrap_or_else(|| Path::new("."));
            network.identity = directory.join(&network.identity);
            for peer in &mut network.peers {
                peer.certificate = directory.join(&peer.certificate);
            }
        }
        Ok(config)
    }

    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let mut config: Self = toml::from_str(source)?;
        if config.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::Version(config.schema_version));
        }
        if config.input.as_ref().is_some_and(|input| {
            input.peer_side.is_some() && input.mode != InputMode::Bidirectional
        }) {
            return Err(ConfigError::InputLayout);
        }
        if config
            .input
            .as_ref()
            .is_some_and(|input| input.allow_locked_receive && input.mode == InputMode::Send)
        {
            return Err(ConfigError::LockedReceiveMode);
        }
        if let Some(network) = &mut config.network {
            match (
                network.peer.take(),
                network.peer_certificate.take(),
                network.peer_fingerprint.take(),
            ) {
                (None, None, None) => {}
                (address, Some(certificate), Some(fingerprint)) if network.peers.is_empty() => {
                    network.peers.push(PeerConfig {
                        address,
                        certificate,
                        fingerprint,
                    });
                }
                _ => return Err(ConfigError::LegacyPeer),
            }
            if network.peers.is_empty() {
                return Err(ConfigError::NoPeers);
            }
            if network.listen.port() == 0
                || network.listen.ip().is_multicast()
                || network.listen.ip() == std::net::Ipv4Addr::BROADCAST
                || network
                    .peers
                    .iter()
                    .filter_map(|peer| peer.address)
                    .any(|address| {
                        let ip = address.ip();
                        network.listen.is_ipv4() != address.is_ipv4()
                            || address.port() == 0
                            || ip.is_unspecified()
                            || ip.is_multicast()
                            || ip == std::net::Ipv4Addr::BROADCAST
                    })
            {
                return Err(ConfigError::NetworkAddress);
            }
            if network.discovery {
                let ip = network.listen.ip();
                if ip.is_unspecified() || ip.is_loopback() {
                    return Err(ConfigError::DiscoveryAddress);
                }
            } else if network.peers.iter().any(|peer| peer.address.is_none()) {
                return Err(ConfigError::PeerAddressRequired);
            }
        }
        if let Some(input) = &config.input
            && !config.network.as_ref().is_some_and(|network| {
                network.peers.iter().any(|peer| {
                    peer.fingerprint
                        .eq_ignore_ascii_case(&input.peer_fingerprint)
                })
            })
        {
            return Err(ConfigError::InputPeer);
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError, LogLevel};

    #[test]
    fn parses_explicit_and_default_log_levels() {
        assert_eq!(
            Config::parse("schema_version = 1").unwrap(),
            Config::default()
        );
        assert_eq!(
            Config::parse("schema_version = 1\nlog_level = 'debug'")
                .unwrap()
                .log_level,
            LogLevel::Debug
        );
    }

    #[test]
    fn input_requires_a_separately_approved_peer_and_is_disabled_by_default() {
        assert!(Config::default().input.is_none());
        let input = "[input]\nmode = 'bidirectional'\npeer_fingerprint = 'ab'";
        let base = "schema_version = 1\n";
        assert!(matches!(
            Config::parse(&format!("{base}{input}")),
            Err(ConfigError::InputPeer)
        ));
        let network = "[network]\nlisten = '127.0.0.1:24800'\nidentity = 'identity.bin'\npeer = '127.0.0.1:24801'\npeer_certificate = 'peer.der'\npeer_fingerprint = 'AB'\n";
        assert!(
            !Config::parse(&format!("{base}{network}{input}"))
                .unwrap()
                .input
                .unwrap()
                .allow_locked_receive
        );
        assert!(
            Config::parse(&format!(
                "{base}{network}{input}\nallow_locked_receive = true"
            ))
            .unwrap()
            .input
            .unwrap()
            .allow_locked_receive
        );
        assert!(matches!(
            Config::parse(&format!(
                "{base}{network}{}\nallow_locked_receive = true",
                input.replace("'bidirectional'", "'send'")
            )),
            Err(ConfigError::LockedReceiveMode)
        ));
        assert_eq!(
            Config::parse(&format!("{base}{network}{input}"))
                .unwrap()
                .input
                .unwrap()
                .mode,
            super::InputMode::Bidirectional
        );
        assert!(matches!(
            Config::parse(&format!("{base}{network}{}", input.replace("'ab'", "'cd'"))),
            Err(ConfigError::InputPeer)
        ));
        let edge = "\npeer_side = 'left'";
        assert_eq!(
            Config::parse(&format!("{base}{network}{input}{edge}"))
                .unwrap()
                .input
                .unwrap()
                .peer_side,
            Some(super::PeerSide::Left)
        );
        let receive = input.replace("'bidirectional'", "'receive'");
        assert!(matches!(
            Config::parse(&format!("{base}{network}{receive}{edge}")),
            Err(ConfigError::InputLayout)
        ));
    }

    #[test]
    fn rejects_incompatible_configuration() {
        assert!(matches!(
            Config::parse("schema_version = 2"),
            Err(ConfigError::Version(2))
        ));
        for source in [
            "",
            "schema_version = 1\nunknown = true",
            "schema_version = 1\nlog_level = 5",
        ] {
            assert!(matches!(Config::parse(source), Err(ConfigError::Toml(_))));
        }
    }

    #[test]
    fn validates_manual_peer_address_and_preserves_local_only_defaults() {
        let settings = r#"
schema_version = 1
[network]
listen = "0.0.0.0:24800"
peer = "192.168.1.10:24800"
identity = "identity.bin"
peer_certificate = "peer.der"
peer_fingerprint = "fingerprint"
"#;
        assert!(Config::default().network.is_none());
        assert_eq!(
            Config::parse(settings).unwrap().network.unwrap().peers[0]
                .address
                .unwrap()
                .port(),
            24800
        );
        for invalid in [
            "0.0.0.0:24800",
            "192.168.1.10:0",
            "224.0.0.1:24800",
            "[::1]:24800",
        ] {
            assert!(matches!(
                Config::parse(&settings.replace("192.168.1.10:24800", invalid)),
                Err(ConfigError::NetworkAddress)
            ));
        }
    }

    #[test]
    fn discovery_allows_omitting_the_manual_peer_on_a_specific_lan_address() {
        let settings = r#"
schema_version = 1
[network]
listen = "192.168.1.7:24800"
discovery = true
identity = "identity.bin"
peer_certificate = "peer.der"
peer_fingerprint = "fingerprint"
"#;
        let network = Config::parse(settings).unwrap().network.unwrap();
        assert!(network.discovery);
        assert!(network.peers[0].address.is_none());
        assert!(matches!(
            Config::parse(&settings.replace("discovery = true", "discovery = false")),
            Err(ConfigError::PeerAddressRequired)
        ));
        for invalid in ["0.0.0.0:24800", "127.0.0.1:24800"] {
            assert!(matches!(
                Config::parse(&settings.replace("192.168.1.7:24800", invalid)),
                Err(ConfigError::DiscoveryAddress)
            ));
        }
    }

    #[test]
    fn parses_multiple_peers_and_rejects_ambiguous_or_missing_approval() {
        let source = r#"
schema_version = 1
[network]
listen = "192.168.1.7:24800"
discovery = true
identity = "identity.bin"

[[network.peers]]
address = "192.168.1.8:24800"
certificate = "a.der"
fingerprint = "a"

[[network.peers]]
certificate = "b.der"
fingerprint = "b"
"#;
        let network = Config::parse(source).unwrap().network.unwrap();
        assert_eq!(network.peers.len(), 2);
        assert!(network.peers[0].address.is_some());
        assert!(network.peers[1].address.is_none());
        let mixed = source.replace(
            "identity = \"identity.bin\"",
            "identity = \"identity.bin\"\npeer = \"192.168.1.9:24800\"",
        );
        assert!(matches!(
            Config::parse(&mixed),
            Err(ConfigError::LegacyPeer)
        ));
        let empty = source.split("[[network.peers]]").next().unwrap();
        assert!(matches!(Config::parse(empty), Err(ConfigError::NoPeers)));
    }
}
