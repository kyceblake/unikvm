//! Standalone native probes for receiving and starting file drags at a screen edge.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::run;
#[cfg(target_os = "windows")]
pub use windows::run;

use std::path::{Path, PathBuf};

/// File representation offered by the probe when starting a native drag.
pub enum Source<'a> {
    File(&'a Path),
    Deferred(&'a Path),
}

fn report(paths: Option<Vec<PathBuf>>) {
    match paths {
        Some(paths) => println!(
            "Native drag exposed {} file reference(s) before release.",
            paths.len()
        ),
        None => println!("No file references were available before release."),
    }
}

fn report_drop(copied: bool) {
    if copied {
        println!("Native drop target accepted a file copy.");
    } else {
        println!("Native drag ended without an accepted copy.");
    }
}
