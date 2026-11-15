//! Standalone native probes for receiving and starting file drags at a screen edge.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::run;
#[cfg(target_os = "windows")]
pub fn run(edge: unikvm_core::geometry::Edge, source: Option<Source<'_>>) -> std::io::Result<()> {
    crate::file_drop::run_probe(edge, source)
}

use std::path::{Path, PathBuf};

/// File representation offered by the probe when starting a native drag.
pub enum Source<'a> {
    File(&'a Path),
    Deferred(&'a Path),
}

pub(crate) fn report(paths: Option<Vec<PathBuf>>) {
    match paths {
        Some(paths) => println!(
            "Native drag exposed {} file reference(s) before release.",
            paths.len()
        ),
        None => println!("No file references were available before release."),
    }
}

pub(crate) fn report_drop(copied: bool) {
    if copied {
        println!("Native drop target accepted a file copy.");
    } else {
        println!("Native drag ended without an accepted copy.");
    }
}
