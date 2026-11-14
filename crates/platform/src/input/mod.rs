//! Native input adapters. Event types are shared with the portable control model.

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod capture;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_capture;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
mod windows_sink;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use capture::{Capture, Shortcut};
#[cfg(target_os = "macos")]
pub use macos::{MacInputSink, MacInputSink as InputSink};
#[cfg(target_os = "macos")]
pub use macos_capture::request_monitor_permission;
#[cfg(target_os = "windows")]
pub use windows_sink::WindowsInputSink as InputSink;
