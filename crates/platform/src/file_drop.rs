//! Native file drag delivery through OLE and AppKit.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::run_received;
#[cfg(target_os = "windows")]
pub use windows::{observe, run_received, run_received_return};

#[cfg(all(target_os = "windows", feature = "file-drag-probe"))]
pub(crate) use windows::run_probe;
