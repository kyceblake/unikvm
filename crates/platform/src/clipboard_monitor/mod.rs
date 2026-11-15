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
    pub bytes: u64,
    pub total: u64,
}

impl Row {
    fn message(self) -> std::borrow::Cow<'static, str> {
        if self.copying && self.total > 0 {
            format!(
                "{} {:.2} / {:.2} MB",
                self.text,
                self.bytes as f64 / 1_000_000.0,
                self.total as f64 / 1_000_000.0
            )
            .into()
        } else {
            self.text.into()
        }
    }
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

/// Display a native prompt on the receiving desktop. Closing it declines the offer.
pub fn approve(label: &str, size: u64) -> io::Result<bool> {
    let size = format!("{:.2} MB", size as f64 / 1_000_000.0);
    #[cfg(target_os = "windows")]
    return windows::approve(label, &size);
    #[cfg(target_os = "macos")]
    return macos::approve(label, &size);
}
