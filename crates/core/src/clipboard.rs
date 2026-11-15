//! Clipboard representations shared by the wire and native adapters.

use std::path::PathBuf;

pub const AUTO_BYTES: u64 = 5_000_000;
pub const MAX_BYTES: u64 = 50_000_000;
pub const MAX_FILES: usize = 32;
pub const MAX_TEXT_BYTES: usize = MAX_BYTES as usize;
pub const MAX_HTML_BYTES: usize = MAX_BYTES as usize;
pub const MAX_PNG_BYTES: usize = MAX_BYTES as usize;
pub const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone)]
pub enum Content {
    Text { plain: String, html: Option<String> },
    Png(Vec<u8>),
    Files(Vec<PathBuf>),
    TooLarge,
}
