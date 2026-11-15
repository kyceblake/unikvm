//! Native Windows file drag observation and delivery through the Shell data object.

mod windows;

pub use windows::{observe, run_received};

#[cfg(feature = "file-drag-probe")]
pub(crate) use windows::run_probe;
