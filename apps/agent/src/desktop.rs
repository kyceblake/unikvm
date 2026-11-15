use anyhow::Result;
use tokio::sync::{broadcast, watch};
use unikvm_config::Config;
use unikvm_network::identity::Fingerprint;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use unikvm_protocol::tray::Transfer;
use unikvm_protocol::tray::{Connection, Peer, Phase, View};

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod file_drop;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) use file_drop::{Drag, NativeDrops};

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Clone)]
pub(crate) struct OutboundDrag {
    pub route: Drag,
    pub path: std::path::PathBuf,
}

#[derive(Clone)]
pub struct Desktop {
    pub view: watch::Sender<View>,
    pub cancels: broadcast::Sender<u64>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub file_drop: NativeDrops,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub drag_offers: broadcast::Sender<OutboundDrag>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub drag_starts: broadcast::Sender<std::path::PathBuf>,
}

impl Desktop {
    pub fn new(config: &Config) -> Result<(Self, Vec<Fingerprint>)> {
        let mut pins = Vec::new();
        let mut view = View::default();
        if let Some(network) = &config.network {
            view.connection = Connection::Connecting;
            for peer in &network.peers {
                let pin: Fingerprint = peer.fingerprint.parse()?;
                pins.push(pin);
                view.peers.push(Peer {
                    name: peer
                        .name
                        .clone()
                        .unwrap_or_else(|| pin.to_string()[..8].to_owned()),
                    connected: false,
                });
            }
        }
        Ok((
            Self {
                view: watch::channel(view).0,
                cancels: broadcast::channel(4).0,
                #[cfg(any(target_os = "windows", target_os = "macos"))]
                file_drop: NativeDrops::default(),
                #[cfg(any(target_os = "windows", target_os = "macos"))]
                drag_offers: broadcast::channel(2).0,
                #[cfg(any(target_os = "windows", target_os = "macos"))]
                drag_starts: broadcast::channel(2).0,
            },
            pins,
        ))
    }

    pub fn cancel(&self, id: u64) -> bool {
        self.view
            .borrow()
            .transfers
            .iter()
            .flatten()
            .any(|transfer| transfer.id == id && transfer.phase == Phase::Copying)
            && self.cancels.send(id).is_ok()
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn transfer(&self, direction: usize, transfer: Transfer) {
        self.view
            .send_modify(|view| view.transfers[direction] = Some(transfer));
    }

    pub fn finish_transfers(&self) {
        self.view.send_modify(|view| {
            for transfer in view.transfers.iter_mut().flatten() {
                if transfer.phase == Phase::Copying {
                    transfer.phase = Phase::Cancelled;
                }
            }
        });
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn update_transfer(&self, direction: usize, id: u64, update: impl FnOnce(&mut Transfer)) {
        self.view.send_modify(|view| {
            if let Some(transfer) = view.transfers[direction]
                .as_mut()
                .filter(|transfer| transfer.id == id && transfer.phase == Phase::Copying)
            {
                update(transfer);
            }
        });
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub struct Helper(std::process::Child);

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Helper {
    pub fn start() -> std::io::Result<Self> {
        Self::launch(&["tray", "--agent"]).map(Self)
    }

    pub fn launch(args: &[&str]) -> std::io::Result<std::process::Child> {
        use std::process::{Command, Stdio};
        let binary = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "unikvm.exe"
        } else {
            "unikvm"
        });
        let mut command = Command::new(binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command.spawn()
    }

    pub fn source(side: &str) -> std::io::Result<Self> {
        Self::launch(&["drag-source", side]).map(Self)
    }

    pub async fn stop(mut self) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Drop provides bounded cleanup if the desktop is no longer responding.
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Drop for Helper {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn cancellation_only_reaches_the_active_transfer_id() {
        let (desktop, _) = Desktop::new(&Config::default()).unwrap();
        let mut cancels = desktop.cancels.subscribe();
        desktop.transfer(
            0,
            Transfer {
                id: 42,
                peer: "Paired computer".to_owned(),
                label: "file.txt".to_owned(),
                items: 1,
                phase: Phase::Copying,
                bytes: 0,
                total: 10,
            },
        );
        assert!(!desktop.cancel(41));
        assert!(desktop.cancel(42));
        assert_eq!(cancels.try_recv().unwrap(), 42);
        desktop.update_transfer(0, 42, |transfer| transfer.phase = Phase::Ready);
        assert!(!desktop.cancel(42));
        assert!(cancels.try_recv().is_err());
    }
}
