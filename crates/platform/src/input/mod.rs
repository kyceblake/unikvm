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
pub(crate) mod macos_capture;
#[cfg(target_os = "macos")]
mod macos_cursor;
#[cfg(all(target_os = "macos", feature = "macos-file-drag"))]
pub(crate) use macos_cursor::is_visible as cursor_is_visible;
#[cfg(target_os = "macos")]
mod macos_edge;
#[cfg(target_os = "macos")]
mod macos_motion;
#[cfg(target_os = "macos")]
pub(crate) mod macos_session;

#[cfg(target_os = "windows")]
pub(crate) mod windows;
#[cfg(target_os = "windows")]
mod windows_cursor;
#[cfg(target_os = "windows")]
mod windows_edge;
#[cfg(target_os = "windows")]
mod windows_motion;
#[cfg(target_os = "windows")]
mod windows_raw_mouse;
#[cfg(target_os = "windows")]
mod windows_sink;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use capture::{Capture, CaptureControl, Shortcut};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use edge::EdgeLayout;
#[cfg(target_os = "macos")]
pub use macos::{MacInputSink, MacInputSink as InputSink};
#[cfg(target_os = "macos")]
pub use macos_capture::request_monitor_permission;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use motion::MotionPacer;
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Debug, Clone, Copy)]
pub struct NativePostTiming {
    pub access: std::time::Duration,
    pub prepare: std::time::Duration,
    pub post: std::time::Duration,
}
#[cfg(all(target_os = "windows", feature = "windows-file-drop"))]
pub(crate) use windows_sink::EVENT_TAG;
#[cfg(target_os = "windows")]
pub use windows_sink::WindowsInputSink as InputSink;
