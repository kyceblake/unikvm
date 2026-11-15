//! Operating system integration for unikvm.

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod clipboard;

#[cfg(all(
    feature = "file-drag-probe",
    any(target_os = "macos", target_os = "windows")
))]
pub mod drag_probe;

#[cfg(all(target_os = "windows", feature = "windows-file-drop"))]
pub mod file_drop;

#[cfg(all(target_os = "macos", feature = "macos-file-drag"))]
pub mod file_drag_source;

#[cfg(all(
    feature = "clipboard-monitor",
    any(target_os = "macos", target_os = "windows")
))]
pub mod clipboard_monitor;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod local;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod credential;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod input;
