//! Native Windows delivery of a verified file through the Shell drag data object.

mod windows;

pub use windows::run_received;

#[cfg(feature = "file-drag-probe")]
pub(crate) use windows::run_probe;
