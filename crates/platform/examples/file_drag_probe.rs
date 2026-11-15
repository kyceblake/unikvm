//! Inspect native file drag metadata on an interactive desktop.

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn main() -> std::io::Result<()> {
    use unikvm_core::geometry::Edge;
    let mut args = std::env::args().skip(1);
    let edge = match args.next().as_deref() {
        None | Some("--left") => Edge::Left,
        Some("--right") => Edge::Right,
        _ => {
            return Err(std::io::Error::other(
                "usage: file_drag_probe [--left | --right]",
            ));
        }
    };
    if args.next().is_some() {
        return Err(std::io::Error::other(
            "usage: file_drag_probe [--left | --right]",
        ));
    }
    unikvm_platform::drag_probe::run(edge)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn main() -> std::process::ExitCode {
    eprintln!("The file drag probe requires Windows or macOS.");
    std::process::ExitCode::FAILURE
}
