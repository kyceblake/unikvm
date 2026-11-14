//! Operating system integration for unikvm.

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod local;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod credential;
