//! Native clipboard status windows, driven by a read-only view from the CLI.

use std::io;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub text: &'static str,
    pub copying: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub connection: &'static str,
    /// Outgoing followed by incoming.
    pub rows: [Row; 2],
}

const TITLE: &str = "UniKVM Clipboard";
const HEADINGS: [&str; 2] = ["To the other computer", "To this computer"];

/// Run the desktop event loop on the calling (main) thread until the window closes.
/// The view callback must return promptly and must not perform IPC.
pub fn show(view: impl Fn() -> View + 'static) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    return windows::show(view);
    #[cfg(target_os = "macos")]
    return macos::show(view);
}
