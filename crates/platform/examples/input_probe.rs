//! Run from an interactive desktop. See docs/platform-support.md.

#[cfg(target_os = "macos")]
#[path = "input_probe/macos.rs"]
mod macos;

#[cfg(target_os = "windows")]
#[path = "input_probe/windows.rs"]
mod windows;

#[cfg(target_os = "windows")]
fn main() -> std::io::Result<()> {
    windows::run()
}

#[cfg(target_os = "macos")]
fn main() -> std::io::Result<()> {
    macos::run()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn main() -> std::process::ExitCode {
    eprintln!("This probe requires a Windows or macOS desktop.");
    std::process::ExitCode::FAILURE
}
