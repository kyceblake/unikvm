#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use {
    tokio::sync::{mpsc, watch},
    unikvm_config::ClipboardConfig,
    unikvm_network::PeerEndpoint,
    unikvm_protocol::local::{Approval, ApprovalDecision, ClipboardStatus},
};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn start(
    _: &ClipboardConfig,
    _: Option<&PeerEndpoint>,
    _: watch::Sender<ClipboardStatus>,
    _: watch::Sender<Option<Approval>>,
    _: mpsc::Receiver<ApprovalDecision>,
) -> anyhow::Result<()> {
    anyhow::bail!("clipboard sharing is available on Windows and macOS");
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod approval;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod staging;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod sync;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use sync::start;
