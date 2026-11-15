//! Bounded, same-user exchange between a native drop helper and its agent.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use unikvm_core::clipboard::MAX_BYTES;

use crate::clipboard::{FileOffer, MAX_NAME_BYTES, valid_name};

use super::{ACK, HEADER, expect, read_flag};

pub(super) const REQUEST: &[u8] = b"UNIKVM/1 DRPDAT\n";
pub const MAX_PATH_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub id: u64,
    pub file: FileOffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Latest,
    Begin(u64),
    Poll(u64),
    Finish { id: u64, copied: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Unavailable,
    Offer(Offer),
    Pending,
    Ready(String),
    Recorded,
}

pub async fn exchange(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    command: Command,
) -> io::Result<Reply> {
    stream.write_all(REQUEST).await?;
    match command {
        Command::Latest => stream.write_u8(0).await?,
        Command::Begin(id) | Command::Poll(id) | Command::Finish { id, .. } => {
            let tag = match command {
                Command::Begin(_) => 1,
                Command::Poll(_) => 2,
                _ => 3,
            };
            stream.write_u8(tag).await?;
            stream.write_u64(id).await?;
            if let Command::Finish { copied, .. } = command {
                stream.write_u8(u8::from(copied)).await?;
            }
        }
    }
    expect(stream, HEADER).await?;
    let reply = match stream.read_u8().await? {
        0 => Reply::Unavailable,
        1 => {
            let id = read_id(stream).await?;
            let size = stream.read_u64().await?;
            let name = read_string(stream, MAX_NAME_BYTES).await?;
            if size > MAX_BYTES || !valid_name(&name) {
                return Err(invalid());
            }
            Reply::Offer(Offer {
                id,
                file: FileOffer { name, size },
            })
        }
        2 => Reply::Pending,
        3 => Reply::Ready(read_string(stream, MAX_PATH_BYTES).await?),
        4 => Reply::Recorded,
        _ => return Err(invalid()),
    };
    stream.write_u8(ACK).await?;
    Ok(reply)
}

impl Command {
    pub(super) async fn read(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Self> {
        Ok(match stream.read_u8().await? {
            0 => Self::Latest,
            1 => Self::Begin(read_id(stream).await?),
            2 => Self::Poll(read_id(stream).await?),
            3 => Self::Finish {
                id: read_id(stream).await?,
                copied: read_flag(stream).await?,
            },
            _ => return Err(invalid()),
        })
    }
}

impl Reply {
    pub(super) async fn write(self, stream: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        match self {
            Self::Unavailable => stream.write_u8(0).await,
            Self::Offer(offer) => {
                if offer.id == 0 || offer.file.size > MAX_BYTES || !valid_name(&offer.file.name) {
                    return Err(invalid());
                }
                stream.write_u8(1).await?;
                stream.write_u64(offer.id).await?;
                stream.write_u64(offer.file.size).await?;
                write_string(stream, &offer.file.name, MAX_NAME_BYTES).await
            }
            Self::Pending => stream.write_u8(2).await,
            Self::Ready(path) => {
                stream.write_u8(3).await?;
                write_string(stream, &path, MAX_PATH_BYTES).await
            }
            Self::Recorded => stream.write_u8(4).await,
        }
    }
}

async fn read_id(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<u64> {
    let id = stream.read_u64().await?;
    if id == 0 { Err(invalid()) } else { Ok(id) }
}

async fn read_string(stream: &mut (impl AsyncRead + Unpin), max: usize) -> io::Result<String> {
    let length = usize::from(stream.read_u16().await?);
    if length == 0 || length > max {
        return Err(invalid());
    }
    let mut value = vec![0; length];
    stream.read_exact(&mut value).await?;
    String::from_utf8(value).map_err(|_| invalid())
}

async fn write_string(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &str,
    max: usize,
) -> io::Result<()> {
    if value.is_empty() || value.len() > max {
        return Err(invalid());
    }
    stream.write_u16(value.len() as u16).await?;
    stream.write_all(value.as_bytes()).await
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid native drop frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{local, tray};

    #[tokio::test]
    async fn same_user_exchange_carries_the_id_and_bounded_staged_path() {
        let cases = [
            (
                Command::Latest,
                Reply::Offer(Offer {
                    id: 42,
                    file: FileOffer {
                        name: "sample.txt".into(),
                        size: 10,
                    },
                }),
            ),
            (Command::Begin(42), Reply::Pending),
            (
                Command::Poll(42),
                Reply::Ready("C:\\staging\\sample.txt".into()),
            ),
            (
                Command::Finish {
                    id: 42,
                    copied: true,
                },
                Reply::Recorded,
            ),
        ];
        for (command, reply) in cases {
            let (mut client, server) = tokio::io::duplex(8);
            let (received, served) = tokio::join!(
                exchange(&mut client, command),
                local::respond_with_desktop(
                    server,
                    || panic!("status request"),
                    || panic!("clipboard request"),
                    |_| None,
                    |_| false,
                    tray::View::default,
                    (
                        |_| false,
                        |received| {
                            assert_eq!(received, command);
                            reply.clone()
                        }
                    ),
                )
            );
            assert_eq!(received.unwrap(), reply);
            served.unwrap();
        }
    }
}
