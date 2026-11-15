//! Bounded status and commands on the existing same-user desktop IPC.

use super::local::ClipboardPhase;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use unikvm_core::clipboard::MAX_BYTES;

pub const STATUS: &[u8] = b"UNIKVM/1 TRYSTS\n";
pub const COMMAND: &[u8] = b"UNIKVM/1 TRYCMD\n";
pub const MAX_PEERS: usize = 16;
pub const MAX_PEER_NAME: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Connection {
    #[default]
    Disabled,
    Disconnected,
    Connecting,
    Online,
    Disconnecting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Copying,
    Ready,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub connection: Connection,
    pub peers: Vec<Peer>,
    /// Sending followed by receiving.
    pub transfers: [Option<Transfer>; 2],
    pub clipboard: [ClipboardPhase; 2],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub id: u64,
    pub peer: String,
    pub label: String,
    pub items: u8,
    pub phase: Phase,
    pub bytes: u64,
    pub total: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Connect,
    Disconnect,
    Exit,
    Cancel(u64),
}

pub fn valid_peer_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= MAX_PEER_NAME && !name.chars().any(char::is_control)
}

pub(crate) async fn write_view(
    writer: &mut (impl AsyncWrite + Unpin),
    view: View,
) -> io::Result<()> {
    if view.peers.len() > MAX_PEERS {
        return Err(invalid());
    }
    writer.write_u8(view.connection as u8).await?;
    for phase in view.clipboard {
        writer.write_u8(phase as u8).await?;
    }
    writer.write_u8(view.peers.len() as u8).await?;
    for peer in view.peers {
        write_string(writer, &peer.name).await?;
        writer.write_u8(u8::from(peer.connected)).await?;
    }
    for transfer in view.transfers {
        writer.write_u8(u8::from(transfer.is_some())).await?;
        if let Some(transfer) = transfer {
            writer.write_u64(transfer.id).await?;
            writer.write_u8(transfer.phase as u8).await?;
            writer.write_u8(transfer.items).await?;
            writer.write_u64(transfer.bytes).await?;
            writer.write_u64(transfer.total).await?;
            write_string(writer, &transfer.peer).await?;
            write_string(writer, &transfer.label).await?;
        }
    }
    Ok(())
}

pub(crate) async fn read_view(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<View> {
    let connection = match reader.read_u8().await? {
        0 => Connection::Disabled,
        1 => Connection::Disconnected,
        2 => Connection::Connecting,
        3 => Connection::Online,
        4 => Connection::Disconnecting,
        _ => return Err(invalid()),
    };
    let clipboard = [
        ClipboardPhase::from_byte(reader.read_u8().await?)?,
        ClipboardPhase::from_byte(reader.read_u8().await?)?,
    ];
    let count = usize::from(reader.read_u8().await?);
    if count > MAX_PEERS {
        return Err(invalid());
    }
    let mut peers = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_string(reader).await?;
        if !valid_peer_name(&name) {
            return Err(invalid());
        }
        peers.push(Peer {
            name,
            connected: super::local::read_flag(reader).await?,
        });
    }
    let mut transfers = [None, None];
    for slot in &mut transfers {
        if super::local::read_flag(reader).await? {
            let id = reader.read_u64().await?;
            let phase = match reader.read_u8().await? {
                0 => Phase::Copying,
                1 => Phase::Ready,
                2 => Phase::Cancelled,
                3 => Phase::Failed,
                _ => return Err(invalid()),
            };
            let items = reader.read_u8().await?;
            let bytes = reader.read_u64().await?;
            let total = reader.read_u64().await?;
            let peer = read_string(reader).await?;
            let label = read_string(reader).await?;
            if id == 0
                || items == 0
                || usize::from(items) > unikvm_core::clipboard::MAX_FILES
                || bytes > total
                || total > MAX_BYTES
                || !valid_peer_name(&peer)
                || label.chars().any(char::is_control)
            {
                return Err(invalid());
            }
            *slot = Some(Transfer {
                id,
                phase,
                items,
                bytes,
                total,
                peer,
                label,
            });
        }
    }
    Ok(View {
        connection,
        peers,
        transfers,
        clipboard,
    })
}

impl Command {
    pub(crate) async fn write(self, writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let tag = match self {
            Self::Connect => 1,
            Self::Disconnect => 2,
            Self::Exit => 3,
            Self::Cancel(_) => 4,
        };
        writer.write_u8(tag).await?;
        if let Self::Cancel(id) = self {
            writer.write_u64(id).await?;
        }
        Ok(())
    }

    pub(crate) async fn read(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Self> {
        match reader.read_u8().await? {
            1 => Ok(Self::Connect),
            2 => Ok(Self::Disconnect),
            3 => Ok(Self::Exit),
            4 => Ok(Self::Cancel(reader.read_u64().await?)),
            _ => Err(invalid()),
        }
    }
}

async fn write_string(writer: &mut (impl AsyncWrite + Unpin), value: &str) -> io::Result<()> {
    writer
        .write_u8(u8::try_from(value.len()).map_err(|_| invalid())?)
        .await?;
    writer.write_all(value.as_bytes()).await
}

async fn read_string(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<String> {
    let mut data = vec![0; usize::from(reader.read_u8().await?)];
    reader.read_exact(&mut data).await?;
    String::from_utf8(data).map_err(|_| invalid())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid tray frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{self, ClipboardPhase, Status};

    #[tokio::test]
    async fn tray_reports_offline_peers_and_binds_cancel_to_a_transfer() {
        let (mut client, server) = tokio::io::duplex(17);
        let expected = View {
            connection: Connection::Online,
            peers: vec![
                Peer {
                    name: "MacBook".into(),
                    connected: true,
                },
                Peer {
                    name: "Windows".into(),
                    connected: false,
                },
            ],
            transfers: [
                None,
                Some(Transfer {
                    id: 42,
                    peer: "MacBook".into(),
                    label: "photo.png".into(),
                    items: 1,
                    phase: Phase::Copying,
                    bytes: 40,
                    total: 100,
                }),
            ],
            clipboard: [ClipboardPhase::Idle, ClipboardPhase::Copying],
        };
        let health = || Status {
            pid: 1,
            uptime_secs: 0,
        };
        let (query, served) = tokio::join!(
            local::query_tray(&mut client),
            local::respond_with_tray(
                server,
                health,
                Default::default,
                |_| None,
                |_| false,
                || expected.clone(),
                |_| false
            ),
        );
        served.unwrap();
        assert_eq!(query.unwrap(), expected);
        for id in [41, 42] {
            let (mut client, server) = tokio::io::duplex(17);
            let (query, served) = tokio::join!(
                local::tray_command(&mut client, Command::Cancel(id)),
                local::respond_with_tray(
                    server,
                    health,
                    Default::default,
                    |_| None,
                    |_| false,
                    View::default,
                    |cmd| cmd == Command::Cancel(42)
                ),
            );
            served.unwrap();
            assert_eq!(query.unwrap(), id == 42);
        }
    }
}
