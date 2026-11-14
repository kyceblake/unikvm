//! Run from an interactive Windows desktop. See docs/platform-support.md.

#[cfg(target_os = "windows")]
#[path = "input_probe/windows.rs"]
mod windows;

#[cfg(target_os = "windows")]
fn main() -> std::io::Result<()> {
    windows::run()
}

#[cfg(not(target_os = "windows"))]
fn main() -> std::process::ExitCode {
    eprintln!("This probe requires a Windows desktop.");
    std::process::ExitCode::FAILURE
}
