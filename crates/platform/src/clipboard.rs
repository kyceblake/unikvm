//! Native clipboard snapshots and conditional writes.

use unikvm_core::clipboard::Content;

#[cfg(target_os = "windows")]
mod cf_html;
mod imaging;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod paste;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{Clipboard, open_source, staging_directory};
#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub use paste::PasteRequests;
#[cfg(target_os = "windows")]
pub use windows::{Clipboard, open_source, staging_directory};

pub struct Snapshot {
    pub sequence: u64,
    pub content: Option<Content>,
    pub imported: bool,
}

pub enum WriteOutcome {
    Applied(u64),
    Changed,
    Unsupported,
}
