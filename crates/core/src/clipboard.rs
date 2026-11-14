//! Clipboard representations shared by the wire and native adapters.

pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_HTML_BYTES: usize = 64 * 1024;
pub const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
pub const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

pub enum Content {
    Text { plain: String, html: Option<String> },
    Png(Vec<u8>),
}
