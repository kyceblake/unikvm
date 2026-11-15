//! Standalone native test for reading a file drag at a screen edge.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::run;
#[cfg(target_os = "windows")]
pub use windows::run;

use std::path::PathBuf;

fn report(paths: Option<Vec<PathBuf>>) {
    match paths {
        Some(paths) => println!(
            "Native drag exposed {} file reference(s) before release.",
            paths.len()
        ),
        None => println!("No file references were available before release."),
    }
}
