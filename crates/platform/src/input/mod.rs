//! Native input adapters. Event types are shared with the portable control model.

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod capture;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod edge;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod motion;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_capture;
#[cfg(target_os = "macos")]
mod macos_cursor;
#[cfg(target_os = "macos")]
mod macos_edge;
#[cfg(target_os = "macos")]
mod macos_motion;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
mod windows_edge;
#[cfg(target_os = "windows")]
mod windows_motion;
#[cfg(target_os = "windows")]
mod windows_raw_mouse;
#[cfg(target_os = "windows")]
mod windows_sink;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use capture::{Capture, Shortcut};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use edge::EdgeLayout;
#[cfg(target_os = "macos")]
pub use macos::{MacInputSink, MacInputSink as InputSink};
#[cfg(target_os = "macos")]
pub use macos_capture::request_monitor_permission;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use motion::MotionPacer;
#[cfg(target_os = "windows")]
pub use windows_sink::WindowsInputSink as InputSink;
