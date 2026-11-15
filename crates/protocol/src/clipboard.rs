//! Clipboard control records. Bulk payloads use a separate QUIC stream.

use std::io;
use std::num::NonZeroU64;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use unikvm_core::clipboard::{AUTO_BYTES, MAX_BYTES, MAX_FILES};
use unikvm_core::device::DeviceId;

pub const READY: &[u8] = b"CLIP/6\n";
pub const DATA: &[u8] = b"CLIP-DATA/6\n";
pub const MAX_NAME_BYTES: usize = 240;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Revision {
    pub counter: NonZeroU64,
    pub origin: DeviceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOffer {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Text { plain: u32, html: u32 },
    Png(u32),
    Files(Vec<FileOffer>),
}

impl Layout {
    pub fn total(&self) -> io::Result<u64> {
        let total = match self {
            Self::Text { plain, html } => u64::from(*plain) + u64::from(*html),
            Self::Png(size) if *size >= 8 => u64::from(*size),
            Self::Files(files) if (1..=MAX_FILES).contains(&files.len()) => {
                files.iter().try_fold(0_u64, |total, file| {
                    if !valid_name(&file.name) {
                        return Err(invalid());
                    }
                    total.checked_add(file.size).ok_or_else(invalid)
                })?
            }
            _ => return Err(invalid()),
        };
        if total > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "clipboard exceeds 50 MB",
            ));
        }
        Ok(total)
    }

    pub fn requires_approval(&self) -> io::Result<bool> {
        Ok(self.total()? > AUTO_BYTES || self.is_file())
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Self::Files(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub revision: Revision,
    pub layout: Layout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Offer(Offer),
    DragOffer { nonce: NonZeroU64, offer: Offer },
    Decision { id: NonZeroU64, accept: bool },
    Receipt { id: NonZeroU64, applied: bool },
    Withdraw(NonZeroU64),
    Request(NonZeroU64),
}

pub fn valid_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.ends_with(['.', ' '])
        || name.chars().any(|ch| {
            ch.is_control()
                || "<>:\"/\\|?*".contains(ch)
                || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return false;
    }
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    !["COM", "LPT"].iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), control: &Control) -> io::Result<()> {
    match control {
        Control::Offer(offer) | Control::DragOffer { offer, .. } => {
            offer.layout.total()?;
            let nonce = match control {
                Control::DragOffer { nonce, .. } => {
                    if !matches!(&offer.layout, Layout::Files(files) if files.len() == 1) {
                        return Err(invalid());
                    }
                    Some(*nonce)
                }
                _ => None,
            };
            writer.write_u8(if nonce.is_some() { 6 } else { 1 }).await?;
            writer.write_u64(offer.revision.counter.get()).await?;
            writer.write_all(&offer.revision.origin.0).await?;
            if let Some(nonce) = nonce {
                writer.write_u64(nonce.get()).await?;
            }
            match &offer.layout {
                Layout::Text { plain, html } => {
                    writer.write_u8(1).await?;
                    writer.write_u32(*plain).await?;
                    writer.write_u32(*html).await?;
                }
                Layout::Png(size) => {
                    writer.write_u8(2).await?;
                    writer.write_u32(*size).await?;
                }
                Layout::Files(files) => {
                    writer.write_u8(3).await?;
                    writer.write_u8(files.len() as u8).await?;
                    for file in files {
                        writer.write_u64(file.size).await?;
                        writer.write_u8(file.name.len() as u8).await?;
                        writer.write_all(file.name.as_bytes()).await?;
                    }
                }
            }
        }
        Control::Decision { id, accept } => write_result(writer, 2, *id, *accept).await?,
        Control::Receipt { id, applied } => write_result(writer, 3, *id, *applied).await?,
        Control::Withdraw(id) | Control::Request(id) => {
            let tag = if matches!(control, Control::Withdraw(_)) {
                4
            } else {
                5
            };
            writer.write_u8(tag).await?;
            writer.write_u64(id.get()).await?;
        }
    }
    Ok(())
}

async fn write_result(
    writer: &mut (impl AsyncWrite + Unpin),
    tag: u8,
    id: NonZeroU64,
    value: bool,
) -> io::Result<()> {
    writer.write_u8(tag).await?;
    writer.write_u64(id.get()).await?;
    writer.write_u8(u8::from(value)).await
}

pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Control> {
    let tag = reader.read_u8().await?;
    let id = NonZeroU64::new(reader.read_u64().await?).ok_or_else(invalid)?;
    match tag {
        1 | 6 => {
            let mut origin = [0; 16];
            reader.read_exact(&mut origin).await?;
            let nonce = if tag == 6 {
                Some(NonZeroU64::new(reader.read_u64().await?).ok_or_else(invalid)?)
            } else {
                None
            };
            let layout = match reader.read_u8().await? {
                1 => Layout::Text {
                    plain: reader.read_u32().await?,
                    html: reader.read_u32().await?,
                },
                2 => Layout::Png(reader.read_u32().await?),
                3 => {
                    let count = reader.read_u8().await? as usize;
                    if !(1..=MAX_FILES).contains(&count) {
                        return Err(invalid());
                    }
                    let mut files = Vec::with_capacity(count);
                    for _ in 0..count {
                        let size = reader.read_u64().await?;
                        let len = reader.read_u8().await? as usize;
                        if !(1..=MAX_NAME_BYTES).contains(&len) {
                            return Err(invalid());
                        }
                        let mut name = vec![0; len];
                        reader.read_exact(&mut name).await?;
                        let name = String::from_utf8(name).map_err(|_| invalid())?;
                        files.push(FileOffer { name, size });
                    }
                    Layout::Files(files)
                }
                _ => return Err(invalid()),
            };
            layout.total()?;
            let offer = Offer {
                revision: Revision {
                    counter: id,
                    origin: DeviceId(origin),
                },
                layout,
            };
            if let Some(nonce) = nonce {
                if !matches!(&offer.layout, Layout::Files(files) if files.len() == 1) {
                    return Err(invalid());
                }
                Ok(Control::DragOffer { nonce, offer })
            } else {
                Ok(Control::Offer(offer))
            }
        }
        2 => Ok(Control::Decision {
            id,
            accept: read_flag(reader).await?,
        }),
        3 => Ok(Control::Receipt {
            id,
            applied: read_flag(reader).await?,
        }),
        4 => Ok(Control::Withdraw(id)),
        5 => Ok(Control::Request(id)),
        _ => Err(invalid()),
    }
}

async fn read_flag(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<bool> {
    match reader.read_u8().await? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid clipboard transfer metadata",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offer_contains_only_metadata_and_round_trips() {
        let offer = Control::Offer(Offer {
            revision: Revision {
                counter: NonZeroU64::MIN,
                origin: DeviceId([2; 16]),
            },
            layout: Layout::Files(vec![FileOffer {
                name: "写真.png".into(),
                size: MAX_BYTES,
            }]),
        });
        let mut wire = Vec::new();
        write(&mut wire, &offer).await.unwrap();
        assert!(wire.len() < 80);
        assert_eq!(read(&mut wire.as_slice()).await.unwrap(), offer);

        let request = Control::Request(NonZeroU64::MIN);
        wire.clear();
        write(&mut wire, &request).await.unwrap();
        assert_eq!(wire.len(), 9);
        assert_eq!(read(&mut wire.as_slice()).await.unwrap(), request);
    }

    #[tokio::test]
    async fn drag_offer_carries_only_one_files_metadata_and_its_session_nonce() {
        let offer = Offer {
            revision: Revision {
                counter: NonZeroU64::MIN,
                origin: DeviceId([7; 16]),
            },
            layout: Layout::Files(vec![FileOffer {
                name: "sample.txt".into(),
                size: MAX_BYTES,
            }]),
        };
        let control = Control::DragOffer {
            nonce: NonZeroU64::new(83).unwrap(),
            offer: offer.clone(),
        };
        let mut wire = Vec::new();
        write(&mut wire, &control).await.unwrap();
        assert!(wire.len() < 80);
        assert_eq!(read(&mut wire.as_slice()).await.unwrap(), control);
        wire[25..33].fill(0);
        assert!(read(&mut wire.as_slice()).await.is_err());
        assert!(
            write(
                &mut Vec::new(),
                &Control::DragOffer {
                    nonce: NonZeroU64::MIN,
                    offer: Offer {
                        layout: Layout::Text { plain: 1, html: 0 },
                        ..offer
                    }
                }
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn thresholds_apply_to_the_total_copy_and_reject_unsafe_names() {
        for size in [AUTO_BYTES, AUTO_BYTES + 1, MAX_BYTES, MAX_BYTES + 1] {
            let layout = Layout::Text {
                plain: (size - 1) as u32,
                html: 1,
            };
            assert_eq!(
                layout.requires_approval().ok(),
                (size <= MAX_BYTES).then_some(size > AUTO_BYTES)
            );
        }
        let file = Layout::Files(vec![FileOffer {
            name: "small.png".into(),
            size: 1,
        }]);
        assert!(file.requires_approval().unwrap());
        for name in [
            "..", "a/../b", "C:secret", "CON.txt", "name.", "name ", "x\\y",
        ] {
            assert!(!valid_name(name));
        }
        assert!(valid_name("résumé.final.pdf"));
    }
}
