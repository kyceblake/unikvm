use anyhow::{Context, Result, bail};
use tokio::sync::watch;
use unikvm_config::{InputConfig, InputMode};
use unikvm_network::{PeerEndpoint, identity::Fingerprint};

#[cfg(target_os = "macos")]
mod receive;
#[cfg(target_os = "windows")]
mod send;

pub struct Control {
    connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    mode: InputMode,
}

impl Control {
    pub fn new(config: &InputConfig, endpoint: Option<&PeerEndpoint>) -> Result<Self> {
        let supported = matches!(config.mode, InputMode::Send) && cfg!(target_os = "windows")
            || matches!(config.mode, InputMode::Receive) && cfg!(target_os = "macos");
        if !supported {
            bail!("input control currently supports send on Windows and receive on macOS");
        }
        let peer = config.peer_fingerprint.parse()?;
        let connections = endpoint
            .and_then(|endpoint| endpoint.connection(peer))
            .context("input peer must be configured in the network trust set")?;
        Ok(Self {
            connections,
            peer,
            mode: config.mode,
        })
    }

    pub async fn run(self) -> Result<()> {
        match self.mode {
            #[cfg(target_os = "windows")]
            InputMode::Send => send::run(self.connections, self.peer).await,
            #[cfg(target_os = "macos")]
            InputMode::Receive => receive::run(self.connections, self.peer).await,
            _ => {
                let _ = (self.connections, self.peer);
                bail!("unsupported input mode")
            }
        }
    }
}
