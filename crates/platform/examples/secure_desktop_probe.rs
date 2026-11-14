//! Explicit Windows service feasibility probe. This executable never injects input.

#[cfg(target_os = "windows")]
#[path = "secure_desktop_probe/windows.rs"]
mod windows;

#[cfg(target_os = "windows")]
fn main() -> std::io::Result<()> {
    windows::run()
}

#[cfg(not(target_os = "windows"))]
fn main() -> std::process::ExitCode {
    eprintln!("The secure desktop probe requires Windows and explicit service setup.");
    std::process::ExitCode::FAILURE
}
