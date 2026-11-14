//! Native input adapters. Event types are shared with the portable control model.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::MacInputSink;
