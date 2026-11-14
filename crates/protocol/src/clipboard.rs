//! Versioned, bounded plain-text clipboard updates.

use std::io;
use std::num::NonZeroU64;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use unikvm_core::device::DeviceId;

const HEADER: &[u8; 8] = b"CLIP/1\r\n";
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

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
}

impl TextUpdate {
    pub fn new(revision: Revision, text: String) -> io::Result<Self> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(invalid("clipboard text exceeds the size limit"));
        }
        if text.contains('\0') {
            return Err(invalid("clipboard text contains an embedded NUL"));
        }
        Ok(Self { revision, text })
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), update: &TextUpdate) -> io::Result<()> {
    let mut metadata = [0; HEADER.len() + 16 + 8 + 4];
    metadata[..8].copy_from_slice(HEADER);
    metadata[8..24].copy_from_slice(&update.revision.origin.0);
    metadata[24..32].copy_from_slice(&update.revision.counter.get().to_be_bytes());
    metadata[32..].copy_from_slice(&(update.text.len() as u32).to_be_bytes());
    writer.write_all(&metadata).await?;
    writer.write_all(update.text.as_bytes()).await
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
    if len > MAX_TEXT_BYTES {
        return Err(invalid("clipboard text exceeds the size limit"));
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload).await?;
    let text = String::from_utf8(payload).map_err(|_| invalid("clipboard text is not UTF-8"))?;
    TextUpdate::new(
        Revision {
            counter,
            origin: DeviceId(origin),
        },
        text,
    )
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
        for (offset, value) in [(0, b'X'), (31, 0), (32, u8::MAX)] {
            let mut bytes = frame.clone();
            bytes[offset] = value;
            let error = read(&mut bytes.as_slice()).await.err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        for value in [0, 255] {
            let mut bytes = frame.clone();
            bytes[35] = 1;
            bytes.push(value);
            let error = read(&mut bytes.as_slice()).await.err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }
}
