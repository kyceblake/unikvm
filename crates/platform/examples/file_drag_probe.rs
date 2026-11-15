//! Test native file drag input and output on an interactive desktop.

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn main() -> std::io::Result<()> {
    use std::{ffi::OsStr, io, path};
    use unikvm_core::geometry::Edge;
    use unikvm_platform::drag_probe::Source;

    let usage = || {
        io::Error::other(
            "usage: file_drag_probe [--left | --right] [--source FILE | --deferred FILE]",
        )
    };
    let mut args = std::env::args_os().skip(1).peekable();
    let edge = match args.peek().and_then(|arg| arg.to_str()) {
        Some("--left") => {
            args.next();
            Edge::Left
        }
        Some("--right") => {
            args.next();
            Edge::Right
        }
        _ => Edge::Left,
    };
    let flag = args.next();
    let path = match flag.as_deref() {
        Some(flag) if flag == OsStr::new("--source") || flag == OsStr::new("--deferred") => {
            let source = path::absolute(args.next().ok_or_else(usage)?)?;
            if !source.symlink_metadata()?.is_file() {
                return Err(io::Error::other("the drag source must be a regular file"));
            }
            Some(source)
        }
        None => None,
        _ => return Err(usage()),
    };
    if args.next().is_some() {
        return Err(usage());
    }
    let source = path.as_deref().map(|path| match flag.as_deref() {
        Some(flag) if flag == OsStr::new("--deferred") => Source::Deferred(path),
        _ => Source::File(path),
    });
    unikvm_platform::drag_probe::run(edge, source)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn main() -> std::process::ExitCode {
    eprintln!("The file drag probe requires Windows or macOS.");
    std::process::ExitCode::FAILURE
}
