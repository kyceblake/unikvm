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
}

/// Explicit local UDP bind and manual peer approval for a single pair.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub listen: SocketAddr,
    pub peer: SocketAddr,
    pub identity: PathBuf,
    pub peer_certificate: PathBuf,
    pub peer_fingerprint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            log_level: LogLevel::default(),
            network: None,
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
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut config = Self::parse(&fs::read_to_string(path)?)?;
        if let Some(network) = &mut config.network {
            let directory = path.parent().unwrap_or_else(|| Path::new("."));
            network.identity = directory.join(&network.identity);
            network.peer_certificate = directory.join(&network.peer_certificate);
        }
        Ok(config)
    }

    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(source)?;
        if config.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::Version(config.schema_version));
        }
        if let Some(network) = &config.network {
            let ip = network.peer.ip();
            if network.listen.is_ipv4() != network.peer.is_ipv4()
                || network.listen.port() == 0
                || network.peer.port() == 0
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip == std::net::Ipv4Addr::BROADCAST
            {
                return Err(ConfigError::NetworkAddress);
            }
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
            Config::parse(settings)
                .unwrap()
                .network
                .unwrap()
                .peer
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
}
