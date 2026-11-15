//! Streaming and staging for clipboard transfers.

use std::fs::{File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use unikvm_core::clipboard::{Content, MAX_BYTES, MAX_FILES, PNG_SIGNATURE};
use unikvm_protocol::clipboard::{FileOffer, Layout, valid_name};

const STALL: Duration = Duration::from_secs(30);
const CHUNK: usize = 64 * 1024;

pub struct Prepared {
    pub layout: Layout,
    source: Source,
}

enum Source {
    Inline { data: Content },
    Files(Vec<(File, Metadata)>),
}

impl Prepared {
    pub fn new(content: Content, open: impl Fn(&Path) -> io::Result<File>) -> io::Result<Self> {
        let (layout, source) = match content {
            Content::TooLarge => return Err(io::ErrorKind::FileTooLarge.into()),
            Content::Files(paths) if (1..=MAX_FILES).contains(&paths.len()) => {
                let mut files = Vec::with_capacity(paths.len());
                let mut items = Vec::with_capacity(paths.len());
                let mut total = 0;
                for (index, path) in paths.iter().enumerate() {
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| valid_name(name))
                        .map_or_else(|| format!("file-{}", index + 1), str::to_owned);
                    let file = open(path)?;
                    let meta = file.metadata()?;
                    if !meta.is_file() {
                        return Err(invalid("clipboard source is not a regular file"));
                    }
                    if meta.len() > MAX_BYTES - total {
                        return Err(io::ErrorKind::FileTooLarge.into());
                    }
                    total += meta.len();
                    items.push(FileOffer {
                        name,
                        size: meta.len(),
                    });
                    files.push((file, meta));
                }
                (Layout::Files(items), Source::Files(files))
            }
            data @ (Content::Text { .. } | Content::Png(_)) => {
                let layout = match &data {
                    Content::Text { plain, html } => {
                        if plain.contains('\0')
                            || html.as_ref().is_some_and(|html| html.contains('\0'))
                        {
                            return Err(invalid("invalid clipboard text"));
                        }
                        Layout::Text {
                            plain: length(plain.len())?,
                            html: length(html.as_ref().map_or(0, String::len))?,
                        }
                    }
                    Content::Png(bytes) if bytes.starts_with(PNG_SIGNATURE) => {
                        Layout::Png(length(bytes.len())?)
                    }
                    _ => return Err(invalid("invalid clipboard image")),
                };
                (layout, Source::Inline { data })
            }
            _ => return Err(invalid("invalid clipboard file selection")),
        };
        layout.total()?;
        Ok(Self { layout, source })
    }
}

fn length(size: usize) -> io::Result<u32> {
    u32::try_from(size).map_err(|_| io::ErrorKind::FileTooLarge.into())
}

pub struct Received {
    pub content: Content,
    pub staging: Option<TempDir>,
}

pub const STAGING_PREFIX: &str = "unikvm-copy-";
const STAGED_BATCHES: usize = 3;

pub async fn send(
    writer: &mut (impl AsyncWrite + Unpin),
    prepared: Prepared,
    mut progress: impl FnMut(u64),
) -> io::Result<()> {
    let mut digest = blake3::Hasher::new();
    let mut bytes_sent = 0;
    match prepared.source {
        Source::Inline { data } => {
            let parts = match &data {
                Content::Text { plain, html } => {
                    [plain.as_bytes(), html.as_deref().unwrap_or("").as_bytes()]
                }
                Content::Png(bytes) => [bytes.as_slice(), &[]],
                _ => unreachable!("prepared inline clipboard content"),
            };
            for chunk in parts.into_iter().flat_map(|part| part.chunks(CHUNK)) {
                timeout(STALL, writer.write_all(chunk)).await??;
                digest.update(chunk);
                bytes_sent += chunk.len() as u64;
                progress(bytes_sent);
            }
        }
        Source::Files(files) => {
            let mut buffer = vec![0; CHUNK];
            for (file, before) in files {
                let mut file = tokio::fs::File::from_std(file);
                let mut remaining = before.len();
                while remaining != 0 {
                    let size = remaining.min(CHUNK as u64) as usize;
                    timeout(STALL, file.read_exact(&mut buffer[..size])).await??;
                    timeout(STALL, writer.write_all(&buffer[..size])).await??;
                    digest.update(&buffer[..size]);
                    remaining -= size as u64;
                    bytes_sent += size as u64;
                    progress(bytes_sent);
                }
                let after = file.metadata().await?;
                if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
                    return Err(invalid("clipboard source changed during transfer"));
                }
            }
        }
    }
    timeout(STALL, writer.write_all(digest.finalize().as_bytes())).await?
}

pub async fn receive(
    reader: &mut (impl AsyncRead + Unpin),
    layout: &Layout,
    directory: Option<&Path>,
    mut progress: impl FnMut(u64),
) -> io::Result<Received> {
    let total = layout.total()?;
    let mut digest = blake3::Hasher::new();
    let mut received = 0;
    let mut advance = |bytes: &[u8]| {
        digest.update(bytes);
        received += bytes.len() as u64;
        progress(received);
    };
    let (content, staging) = match layout {
        Layout::Text { plain, html } => {
            let plain = read_text(reader, *plain, &mut advance).await?;
            let html = if *html == 0 {
                None
            } else {
                Some(read_text(reader, *html, &mut advance).await?)
            };
            (Content::Text { plain, html }, None)
        }
        Layout::Png(size) => {
            let bytes = read_bytes(reader, *size, &mut advance).await?;
            if !bytes.starts_with(PNG_SIGNATURE) {
                return Err(invalid("invalid clipboard image"));
            }
            (Content::Png(bytes), None)
        }
        Layout::Files(files) => {
            let directory = directory.ok_or_else(|| invalid("clipboard staging is unavailable"))?;
            let mut entries = tokio::fs::read_dir(directory).await?;
            let mut batches = 0;
            while let Some(entry) = entries.next_entry().await? {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(STAGING_PREFIX))
                {
                    batches += 1;
                    if batches >= STAGED_BATCHES {
                        return Err(io::Error::other("clipboard temporary storage is full"));
                    }
                }
            }
            let staging = tempfile::Builder::new()
                .prefix(STAGING_PREFIX)
                .tempdir_in(directory)?;
            let paths = receive_files(reader, files, staging.path(), &mut advance).await?;
            (Content::Files(paths), Some(staging))
        }
    };
    let mut expected = [0; 32];
    timeout(STALL, reader.read_exact(&mut expected)).await??;
    if received != total || digest.finalize().as_bytes() != &expected {
        return Err(invalid("clipboard transfer integrity check failed"));
    }
    if timeout(STALL, reader.read(&mut [0; 1])).await?? != 0 {
        return Err(invalid("unexpected clipboard transfer data"));
    }
    Ok(Received { content, staging })
}

async fn read_bytes(
    reader: &mut (impl AsyncRead + Unpin),
    size: u32,
    advance: &mut impl FnMut(&[u8]),
) -> io::Result<Vec<u8>> {
    let mut data = vec![0; size as usize];
    for chunk in data.chunks_mut(CHUNK) {
        timeout(STALL, reader.read_exact(chunk)).await??;
        advance(chunk);
    }
    Ok(data)
}

async fn read_text(
    reader: &mut (impl AsyncRead + Unpin),
    size: u32,
    advance: &mut impl FnMut(&[u8]),
) -> io::Result<String> {
    let bytes = read_bytes(reader, size, advance).await?;
    if bytes.contains(&0) {
        return Err(invalid("invalid clipboard text"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("invalid clipboard text"))
}

async fn receive_files(
    reader: &mut (impl AsyncRead + Unpin),
    files: &[FileOffer],
    staging: &Path,
    advance: &mut impl FnMut(&[u8]),
) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::with_capacity(files.len());
    let mut buffer = vec![0; CHUNK];
    for (index, offer) in files.iter().enumerate() {
        // A private subdirectory per file preserves duplicate basenames.
        let directory = staging.join(index.to_string());
        tokio::fs::create_dir(&directory).await?;
        let path = directory.join(&offer.name);
        let mut file = tokio::fs::File::create_new(&path).await?;
        let mut remaining = offer.size;
        while remaining != 0 {
            let size = remaining.min(CHUNK as u64) as usize;
            let chunk = &mut buffer[..size];
            timeout(STALL, reader.read_exact(chunk)).await??;
            file.write_all(chunk).await?;
            advance(chunk);
            remaining -= size as u64;
        }
        file.flush().await?;
        paths.push(path);
    }
    Ok(paths)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn streams_a_fifty_megabyte_batch_and_preserves_duplicate_names() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("other")).unwrap();
        let first = source.path().join("archive.bin");
        let second = source.path().join("other/archive.bin");
        File::create(&first)
            .unwrap()
            .set_len(MAX_BYTES - 3)
            .unwrap();
        fs::write(&second, b"end").unwrap();
        let paths = [first, second];
        let prepared =
            Prepared::new(Content::Files(paths.to_vec()), |path| File::open(path)).unwrap();
        let layout = prepared.layout.clone();
        assert_eq!(layout.total().unwrap(), MAX_BYTES);
        assert!(layout.requires_approval().unwrap());
        let (mut writer, mut reader) = tokio::io::duplex(16 * 1024);
        let mut sent = 0;
        let mut received = 0;
        let ((), data) = tokio::join!(
            async {
                send(&mut writer, prepared, |bytes| sent = bytes)
                    .await
                    .unwrap();
                writer.shutdown().await.unwrap();
            },
            receive(&mut reader, &layout, Some(target.path()), |bytes| {
                received = bytes
            }),
        );
        let data = data.unwrap();
        assert_eq!((sent, received), (MAX_BYTES, MAX_BYTES));
        let Content::Files(staged) = &data.content else {
            panic!("expected files");
        };
        assert_eq!(staged.len(), 2);
        for (original, new) in paths.iter().zip(staged) {
            assert_eq!(original.file_name(), new.file_name());
            let hash = |path| {
                blake3::Hasher::new()
                    .update_reader(File::open(path).unwrap())
                    .unwrap()
                    .finalize()
            };
            assert_eq!(hash(original), hash(new));
        }
        drop(data);
        assert!(fs::read_dir(target.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn corrupt_and_incomplete_transfers_remove_staged_files() {
        let target = tempfile::tempdir().unwrap();
        let layout = Layout::Files(vec![FileOffer {
            name: "test.bin".into(),
            size: 3,
        }]);
        for mut wire in [b"ab".as_slice(), &[0; 35][..]] {
            let result = receive(&mut wire, &layout, Some(target.path()), |_| {}).await;
            assert!(result.is_err());
            assert!(fs::read_dir(target.path()).unwrap().next().is_none());
        }
    }
}
