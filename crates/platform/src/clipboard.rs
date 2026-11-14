//! Native clipboard snapshots and conditional writes.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::Clipboard;
#[cfg(target_os = "windows")]
pub use windows::Clipboard;

pub struct Snapshot {
    pub sequence: u64,
    pub text: Option<String>,
}

pub enum WriteOutcome {
    Applied(u64),
    Changed,
}
