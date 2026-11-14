//! Versioned, bounded clipboard updates with a plain-text representation.

use std::io;
use std::num::NonZeroU64;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use unikvm_core::device::DeviceId;

const HEADER: &[u8; 8] = b"CLIP/2\r\n";
pub const READY: &[u8; 8] = b"CLIP/2\n\n";
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_HTML_BYTES: usize = 64 * 1024;

/// Order concurrent writes by logical counter, then by device ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision {
    pub counter: NonZeroU64,
    pub origin: DeviceId,
}

// Clipboard contents should never be formatted into diagnostics.
pub struct TextUpdate {
    revision: Revision,
    text: String,
    html: Option<String>,
}

impl TextUpdate {
    pub fn new(revision: Revision, text: String) -> io::Result<Self> {
        Self::with_html(revision, text, None)
    }

    pub fn with_html(revision: Revision, text: String, html: Option<String>) -> io::Result<Self> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(invalid("clipboard text exceeds the size limit"));
        }
        if text.contains('\0') {
            return Err(invalid("clipboard text contains an embedded NUL"));
        }
        if html
            .as_ref()
            .is_some_and(|html| html.len() > MAX_HTML_BYTES || html.contains('\0'))
        {
            return Err(invalid("invalid clipboard HTML"));
        }
        Ok(Self {
            revision,
            text,
            html: html.filter(|html| !html.is_empty()),
        })
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn html(&self) -> Option<&str> {
        self.html.as_deref()
    }
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), update: &TextUpdate) -> io::Result<()> {
    let mut metadata = [0; HEADER.len() + 16 + 8 + 4 + 4];
    metadata[..8].copy_from_slice(HEADER);
    metadata[8..24].copy_from_slice(&update.revision.origin.0);
    metadata[24..32].copy_from_slice(&update.revision.counter.get().to_be_bytes());
    metadata[32..36].copy_from_slice(&(update.text.len() as u32).to_be_bytes());
    let html = update.html().unwrap_or_default();
    metadata[36..].copy_from_slice(&(html.len() as u32).to_be_bytes());
    writer.write_all(&metadata).await?;
    writer.write_all(update.text.as_bytes()).await?;
    if !html.is_empty() {
        writer.write_all(html.as_bytes()).await?;
    }
    Ok(())
}

pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<TextUpdate> {
    let mut header = [0; HEADER.len()];
    reader.read_exact(&mut header).await?;
    if &header != HEADER {
        return Err(invalid("unsupported clipboard frame"));
    }
    let mut origin = [0; 16];
    reader.read_exact(&mut origin).await?;
    let counter = NonZeroU64::new(reader.read_u64().await?)
        .ok_or_else(|| invalid("clipboard revision must be nonzero"))?;
    let len = reader.read_u32().await? as usize;
    let html_len = reader.read_u32().await? as usize;
    if len > MAX_TEXT_BYTES {
        return Err(invalid("clipboard text exceeds the size limit"));
    }
    if html_len > MAX_HTML_BYTES {
        return Err(invalid("clipboard HTML exceeds the size limit"));
    }
    let text = read_utf8(reader, len).await?;
    let html = if html_len != 0 {
        Some(read_utf8(reader, html_len).await?)
    } else {
        None
    };
    TextUpdate::with_html(
        Revision {
            counter,
            origin: DeviceId(origin),
        },
        text,
        html,
    )
}

async fn read_utf8(reader: &mut (impl AsyncRead + Unpin), len: usize) -> io::Result<String> {
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload).await?;
    String::from_utf8(payload).map_err(|_| invalid("clipboard data is not UTF-8"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn revision() -> Revision {
        Revision {
            counter: NonZeroU64::new(7).unwrap(),
            origin: DeviceId([42; 16]),
        }
    }

    #[tokio::test]
    async fn round_trips_unicode_across_partial_reads_and_writes() {
        let expected = TextUpdate::new(revision(), "日本語\nПривет\n🧪".into()).unwrap();
        let (mut sender, mut receiver) = duplex(5);
        let (sent, received) = tokio::join!(write(&mut sender, &expected), read(&mut receiver));
        sent.unwrap();
        let received = received.unwrap();
        assert_eq!(received.revision(), expected.revision());
        assert!(received.text() == expected.text());
        assert!(received.html().is_none());
    }

    #[tokio::test]
    async fn rich_update_keeps_the_html_and_plain_text_in_one_revision() {
        let expected = TextUpdate::with_html(
            revision(),
            "日本語".into(),
            Some("<strong>日本語</strong>".into()),
        )
        .unwrap();
        let (mut sender, mut receiver) = duplex(5);
        let (sent, received) = tokio::join!(write(&mut sender, &expected), read(&mut receiver));
        sent.unwrap();
        let received = received.unwrap();
        assert_eq!(received.revision(), expected.revision());
        assert!(received.text() == expected.text());
        assert!(received.html() == expected.html());
    }

    #[tokio::test]
    async fn rejects_invalid_metadata_before_reading_payload() {
        let mut frame = Vec::new();
        write(
            &mut frame,
            &TextUpdate::new(revision(), String::new()).unwrap(),
        )
        .await
        .unwrap();
        for (offset, value) in [(0, b'X'), (31, 0), (32, u8::MAX), (36, u8::MAX)] {
            let mut bytes = frame.clone();
            bytes[offset] = value;
            let error = read(&mut bytes.as_slice()).await.err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        for length_byte in [35, 39] {
            for value in [0, 255] {
                let mut bytes = frame.clone();
                bytes[length_byte] = 1;
                bytes.push(value);
                let error = read(&mut bytes.as_slice()).await.err().unwrap();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            }
        }
    }
}
