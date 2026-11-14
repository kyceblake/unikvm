//! Fixed-size versioned status exchange for the local IPC stream.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const REQUEST: &[u8] = b"UNIKVM/1 STATUS\n";
const CLIPBOARD: &[u8] = b"UNIKVM/1 CLIPBD\n";
const ACK: u8 = 0x06;
const HEADER: &[u8] = b"UNIKVM/1 OK\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub pid: u32,
    pub uptime_secs: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClipboardPhase {
    #[default]
    Idle,
    Copying,
    Ready,
    Failed,
    Skipped,
}

impl ClipboardPhase {
    fn from_byte(byte: u8) -> io::Result<Self> {
        match byte {
            0 => Ok(Self::Idle),
            1 => Ok(Self::Copying),
            2 => Ok(Self::Ready),
            3 => Ok(Self::Failed),
            4 => Ok(Self::Skipped),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid clipboard status",
            )),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardStatus {
    pub connected: bool,
    pub outgoing: ClipboardPhase,
    pub incoming: ClipboardPhase,
}

pub async fn query(stream: &mut (impl AsyncRead + AsyncWrite + Unpin)) -> io::Result<Status> {
    stream.write_all(REQUEST).await?;
    expect(stream, HEADER).await?;
    let pid = stream.read_u32().await?;
    let uptime_secs = stream.read_u64().await?;
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid agent PID",
        ));
    }
    stream.write_u8(ACK).await?;
    Ok(Status { pid, uptime_secs })
}

pub async fn query_clipboard(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> io::Result<ClipboardStatus> {
    stream.write_all(CLIPBOARD).await?;
    expect(stream, HEADER).await?;
    let mut state = [0; 3];
    stream.read_exact(&mut state).await?;
    let status = ClipboardStatus {
        connected: state[0] == 1,
        outgoing: ClipboardPhase::from_byte(state[1])?,
        incoming: ClipboardPhase::from_byte(state[2])?,
    };
    if state[0] > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid clipboard connection state",
        ));
    }
    stream.write_u8(ACK).await?;
    Ok(status)
}

pub async fn respond(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    status: impl FnOnce() -> Status,
) -> io::Result<()> {
    respond_with_clipboard(stream, status, ClipboardStatus::default).await
}

pub async fn respond_with_clipboard(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    status: impl FnOnce() -> Status,
    clipboard: impl FnOnce() -> ClipboardStatus,
) -> io::Result<()> {
    let mut request = [0; REQUEST.len()];
    stream.read_exact(&mut request).await?;
    match request.as_slice() {
        REQUEST => {
            let status = status();
            let mut response = [0u8; HEADER.len() + 12];
            response[..HEADER.len()].copy_from_slice(HEADER);
            response[HEADER.len()..HEADER.len() + 4].copy_from_slice(&status.pid.to_be_bytes());
            response[HEADER.len() + 4..].copy_from_slice(&status.uptime_secs.to_be_bytes());
            stream.write_all(&response).await?;
        }
        CLIPBOARD => {
            let state = clipboard();
            let mut response = [0; HEADER.len() + 3];
            response[..HEADER.len()].copy_from_slice(HEADER);
            response[HEADER.len()..].copy_from_slice(&[
                u8::from(state.connected),
                state.outgoing as u8,
                state.incoming as u8,
            ]);
            stream.write_all(&response).await?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported local IPC frame",
            ));
        }
    }
    if stream.read_u8().await? != ACK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid status acknowledgment",
        ));
    }
    Ok(())
}

async fn expect(stream: &mut (impl AsyncRead + Unpin), expected: &[u8]) -> io::Result<()> {
    let mut frame = [0u8; REQUEST.len()];
    let frame = &mut frame[..expected.len()];
    stream.read_exact(frame).await?;
    if frame != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported local IPC frame",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;

    use tokio::io::{AsyncWriteExt, duplex};

    use super::{
        ClipboardPhase, ClipboardStatus, Status, query, query_clipboard, respond,
        respond_with_clipboard,
    };

    #[tokio::test]
    async fn clipboard_round_trip_with_partial_reads_and_writes() {
        let (mut client, mut server) = duplex(5);
        let expected = ClipboardStatus {
            connected: true,
            outgoing: ClipboardPhase::Copying,
            incoming: ClipboardPhase::Ready,
        };
        let (reply, sent) = tokio::join!(
            query_clipboard(&mut client),
            respond_with_clipboard(
                &mut server,
                || panic!("unexpected status request"),
                || expected
            ),
        );
        assert_eq!(reply.unwrap(), expected);
        sent.unwrap();
    }

    #[tokio::test]
    async fn status_round_trip_with_partial_reads_and_writes() {
        let (mut client, mut server) = duplex(5);
        let expected = Status {
            pid: 42,
            uptime_secs: 513,
        };
        let (reply, sent) = tokio::join!(query(&mut client), respond(&mut server, || expected));
        assert_eq!(reply.unwrap(), expected);
        sent.unwrap();
    }

    #[tokio::test]
    async fn rejects_unknown_requests_before_building_a_status() {
        let (mut client, mut server) = duplex(32);
        client.write_all(b"UNIKVM/2 STATUS\n").await.unwrap();
        let err = respond(&mut server, || panic!("request validation must come first"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn reports_an_incomplete_reply() {
        let (mut client, mut server) = duplex(32);
        let (reply, ()) = tokio::join!(query(&mut client), async move {
            super::expect(&mut server, super::REQUEST).await.unwrap();
            server.write_all(super::HEADER).await.unwrap();
        });
        assert_eq!(reply.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
