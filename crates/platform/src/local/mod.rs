//! User-scoped local transport. Wire encoding lives in unikvm-protocol.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::{Endpoint, Listener};
#[cfg(target_os = "windows")]
pub use windows::{Endpoint, Listener};

/// Includes both endpoint connection and the status frame exchange.
pub const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(test)]
mod tests;
