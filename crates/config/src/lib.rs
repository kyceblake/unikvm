//! Configuration for the local agent.

use std::fs;
use std::io;
use std::path::Path;

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            log_level: LogLevel::default(),
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
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::parse(&fs::read_to_string(path)?)
    }

    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(source)?;
        if config.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::Version(config.schema_version));
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
}
