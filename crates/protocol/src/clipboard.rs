//! Versioned, bounded text and PNG clipboard updates.

use std::io;
use std::num::NonZeroU64;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use unikvm_core::clipboard::{
    Content, MAX_HTML_BYTES, MAX_PNG_BYTES, MAX_TEXT_BYTES, PNG_SIGNATURE,
};
use unikvm_core::device::DeviceId;

const HEADER: &[u8; 8] = b"CLIP/3\r\n";
pub const READY: &[u8; 8] = b"CLIP/3\n\n";

/// Order concurrent writes by logical counter, then by device ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision {
    pub counter: NonZeroU64,
    pub origin: DeviceId,
}

// Clipboard contents should never be formatted into diagnostics.
pub struct Update {
    revision: Revision,
    content: Content,
}

impl Update {
    pub fn new(revision: Revision, content: Content) -> io::Result<Self> {
        match &content {
            Content::Text { plain, html } => {
                if plain.len() > MAX_TEXT_BYTES || plain.contains('\0') {
                    return Err(invalid("invalid clipboard text"));
                }
                if html
                    .as_ref()
                    .is_some_and(|html| html.len() > MAX_HTML_BYTES || html.contains('\0'))
                {
                    return Err(invalid("invalid clipboard HTML"));
                }
            }
            Content::Png(png) => {
                if png.len() > MAX_PNG_BYTES || !png.starts_with(PNG_SIGNATURE) {
                    return Err(invalid("invalid clipboard PNG"));
                }
            }
        }
        Ok(Self { revision, content })
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn content(&self) -> &Content {
        &self.content
    }
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), update: &Update) -> io::Result<()> {
    let (plain, html, png) = match &update.content {
        Content::Text { plain, html } => (
            plain.as_bytes(),
            html.as_deref().unwrap_or_default().as_bytes(),
            &[][..],
        ),
        Content::Png(png) => (&[][..], &[][..], png.as_slice()),
    };
    let mut metadata = [0; 44];
    metadata[..8].copy_from_slice(HEADER);
    metadata[8..24].copy_from_slice(&update.revision.origin.0);
    metadata[24..32].copy_from_slice(&update.revision.counter.get().to_be_bytes());
    metadata[32..36].copy_from_slice(&(plain.len() as u32).to_be_bytes());
    metadata[36..40].copy_from_slice(&(html.len() as u32).to_be_bytes());
    metadata[40..44].copy_from_slice(&(png.len() as u32).to_be_bytes());
    writer.write_all(&metadata).await?;
    for bytes in [plain, html, png] {
        if !bytes.is_empty() {
            writer.write_all(bytes).await?;
        }
    }
    Ok(())
}

pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Update> {
    let mut header = [0; HEADER.len()];
    reader.read_exact(&mut header).await?;
    if &header != HEADER {
        return Err(invalid("unsupported clipboard frame"));
    }
    let mut origin = [0; 16];
    reader.read_exact(&mut origin).await?;
    let counter = NonZeroU64::new(reader.read_u64().await?)
        .ok_or_else(|| invalid("clipboard revision must be nonzero"))?;
    let plain_len = reader.read_u32().await? as usize;
    let html_len = reader.read_u32().await? as usize;
    let png_len = reader.read_u32().await? as usize;
    if plain_len > MAX_TEXT_BYTES
        || html_len > MAX_HTML_BYTES
        || png_len > MAX_PNG_BYTES
        || (png_len != 0 && (plain_len != 0 || html_len != 0))
    {
        return Err(invalid("invalid clipboard representation lengths"));
    }

    let content = if png_len != 0 {
        Content::Png(read_bytes(reader, png_len).await?)
    } else {
        let plain = read_utf8(reader, plain_len).await?;
        let html = if html_len != 0 {
            Some(read_utf8(reader, html_len).await?)
        } else {
            None
        };
        Content::Text { plain, html }
    };
    Update::new(
        Revision {
            counter,
            origin: DeviceId(origin),
        },
        content,
    )
}

async fn read_bytes(reader: &mut (impl AsyncRead + Unpin), len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn read_utf8(reader: &mut (impl AsyncRead + Unpin), len: usize) -> io::Result<String> {
    String::from_utf8(read_bytes(reader, len).await?)
        .map_err(|_| invalid("clipboard data is not UTF-8"))
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

    async fn round_trip(content: Content) -> Update {
        let expected = Update::new(revision(), content).unwrap();
        let (mut sender, mut receiver) = duplex(5);
        let (sent, received) = tokio::join!(write(&mut sender, &expected), read(&mut receiver));
        sent.unwrap();
        let received = received.unwrap();
        assert_eq!(received.revision(), revision());
        received
    }

    #[tokio::test]
    async fn round_trips_unicode_and_optional_html_across_partial_io() {
        let text = "日本語\nПривет\n🧪";
        for rich in [None, Some("<strong>日本語</strong>")] {
            let received = round_trip(Content::Text {
                plain: text.into(),
                html: rich.map(str::to_owned),
            })
            .await;
            assert!(matches!(received.content(), Content::Text { plain, html }
                if plain == text && html.as_deref() == rich));
        }
    }

    #[tokio::test]
    async fn round_trips_png_bytes_as_a_separate_representation() {
        let mut bytes = PNG_SIGNATURE.to_vec();
        bytes.extend([0, 1, 2, 3]);
        let received = round_trip(Content::Png(bytes.clone())).await;
        assert!(matches!(received.content(), Content::Png(data) if data == &bytes));
    }

    #[tokio::test]
    async fn rejects_invalid_metadata_before_reading_payload() {
        let mut frame = Vec::new();
        let empty = Update::new(
            revision(),
            Content::Text {
                plain: String::new(),
                html: None,
            },
        )
        .unwrap();
        write(&mut frame, &empty).await.unwrap();
        for (offset, value) in [
            (0, b'X'),
            (31, 0),
            (32, u8::MAX),
            (36, u8::MAX),
            (40, u8::MAX),
        ] {
            let mut bytes = frame.clone();
            bytes[offset] = value;
            let error = read(&mut bytes.as_slice()).await.err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        let mut mixed = frame.clone();
        mixed[35] = 1;
        mixed[43] = 1;
        assert_eq!(
            read(&mut mixed.as_slice()).await.err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );

        for length_byte in [35, 39, 43] {
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
