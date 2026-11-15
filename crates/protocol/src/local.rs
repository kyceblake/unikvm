//! Fixed-size versioned status exchange for the local IPC stream.

use std::io;

use crate::tray;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const REQUEST: &[u8] = b"UNIKVM/1 STATUS\n";
const CLIPBOARD: &[u8] = b"UNIKVM/2 CLIPBD\n";
const LEGACY_CLIPBOARD: &[u8] = b"UNIKVM/1 CLIPBD\n";
const OFFER: &[u8] = b"UNIKVM/1 CLPOFR\n";
const DECIDE: &[u8] = b"UNIKVM/1 CLPDEC\n";
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
    Waiting,
    TooLarge,
    Declined,
    Offered,
}

impl ClipboardPhase {
    pub(crate) fn from_byte(byte: u8) -> io::Result<Self> {
        match byte {
            0 => Ok(Self::Idle),
            1 => Ok(Self::Copying),
            2 => Ok(Self::Ready),
            3 => Ok(Self::Failed),
            4 => Ok(Self::Skipped),
            5 => Ok(Self::Waiting),
            6 => Ok(Self::TooLarge),
            7 => Ok(Self::Declined),
            8 => Ok(Self::Offered),
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
    pub sent: Progress,
    pub received: Progress,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub bytes: u64,
    pub total: u64,
}

#[derive(Clone)]
pub struct Approval {
    pub id: u64,
    pub bytes: u64,
    pub label: String,
}

#[derive(Clone, Copy)]
pub struct ApprovalDecision {
    pub id: u64,
    pub accept: bool,
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
    read_clipboard_status(stream, CLIPBOARD).await
}

pub async fn query_clipboard_legacy(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> io::Result<ClipboardStatus> {
    read_clipboard_status(stream, LEGACY_CLIPBOARD).await
}

async fn read_clipboard_status(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    request: &[u8],
) -> io::Result<ClipboardStatus> {
    stream.write_all(request).await?;
    expect(stream, HEADER).await?;
    let mut state = [0; 3];
    stream.read_exact(&mut state).await?;
    let status = ClipboardStatus {
        connected: state[0] == 1,
        outgoing: ClipboardPhase::from_byte(state[1])?,
        incoming: ClipboardPhase::from_byte(state[2])?,
        sent: read_progress(stream).await?,
        received: read_progress(stream).await?,
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

async fn read_progress(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Progress> {
    let bytes = stream.read_u64().await?;
    let total = stream.read_u64().await?;
    if total > unikvm_core::clipboard::MAX_BYTES || bytes > total {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid clipboard progress",
        ));
    }
    Ok(Progress { bytes, total })
}

pub async fn approval(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    id: u64,
) -> io::Result<Option<Approval>> {
    stream.write_all(OFFER).await?;
    stream.write_u64(id).await?;
    expect(stream, HEADER).await?;
    let offer = if read_flag(stream).await? {
        let bytes = stream.read_u64().await?;
        let len = stream.read_u8().await? as usize;
        let mut label = vec![0; len];
        stream.read_exact(&mut label).await?;
        let label = String::from_utf8(label).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid clipboard offer label")
        })?;
        Some(Approval { id, bytes, label })
    } else {
        None
    };
    stream.write_u8(ACK).await?;
    Ok(offer)
}

pub async fn decide(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    decision: ApprovalDecision,
) -> io::Result<bool> {
    stream.write_all(DECIDE).await?;
    stream.write_u64(decision.id).await?;
    stream.write_u8(u8::from(decision.accept)).await?;
    expect(stream, HEADER).await?;
    let accepted = read_flag(stream).await?;
    stream.write_u8(ACK).await?;
    Ok(accepted)
}

pub(crate) async fn read_flag(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<bool> {
    match stream.read_u8().await? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid clipboard flag",
        )),
    }
}

pub async fn respond(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    status: impl FnOnce() -> Status,
) -> io::Result<()> {
    respond_with_clipboard(stream, status, ClipboardStatus::default).await
}

pub async fn respond_with_clipboard(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    status: impl FnOnce() -> Status,
    clipboard: impl FnOnce() -> ClipboardStatus,
) -> io::Result<()> {
    respond_with_transfers(stream, status, clipboard, |_| None, |_| false).await
}

pub async fn respond_with_transfers(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    status: impl FnOnce() -> Status,
    clipboard: impl FnOnce() -> ClipboardStatus,
    offer: impl FnOnce(u64) -> Option<Approval>,
    decide: impl FnOnce(ApprovalDecision) -> bool,
) -> io::Result<()> {
    respond_with_tray(
        stream,
        status,
        clipboard,
        offer,
        decide,
        tray::View::default,
        |_| false,
    )
    .await
}

pub async fn query_tray(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> io::Result<tray::View> {
    stream.write_all(tray::STATUS).await?;
    expect(stream, HEADER).await?;
    let view = tray::read_view(stream).await?;
    stream.write_u8(ACK).await?;
    Ok(view)
}

pub async fn tray_command(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    command: tray::Command,
) -> io::Result<bool> {
    stream.write_all(tray::COMMAND).await?;
    command.write(stream).await?;
    expect(stream, HEADER).await?;
    let accepted = read_flag(stream).await?;
    stream.write_u8(ACK).await?;
    Ok(accepted)
}

pub async fn respond_with_tray(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    status: impl FnOnce() -> Status,
    clipboard: impl FnOnce() -> ClipboardStatus,
    offer: impl FnOnce(u64) -> Option<Approval>,
    decide: impl FnOnce(ApprovalDecision) -> bool,
    tray_view: impl FnOnce() -> tray::View,
    tray_command: impl FnOnce(tray::Command) -> bool,
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
        OFFER => {
            let id = stream.read_u64().await?;
            let offer = offer(id);
            stream.write_all(HEADER).await?;
            stream.write_u8(u8::from(offer.is_some())).await?;
            if let Some(offer) = offer {
                let len = u8::try_from(offer.label.len()).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "clipboard offer label is too long",
                    )
                })?;
                stream.write_u64(offer.bytes).await?;
                stream.write_u8(len).await?;
                stream.write_all(offer.label.as_bytes()).await?;
            }
        }
        DECIDE => {
            let id = stream.read_u64().await?;
            let accept = read_flag(&mut stream).await?;
            let queued = decide(ApprovalDecision { id, accept });
            stream.write_all(HEADER).await?;
            stream.write_u8(u8::from(queued)).await?;
        }
        CLIPBOARD | LEGACY_CLIPBOARD => {
            let state = clipboard();
            let encode_phase = |phase| match (request.as_slice(), phase) {
                (LEGACY_CLIPBOARD, ClipboardPhase::Offered) => ClipboardPhase::Waiting as u8,
                (_, phase) => phase as u8,
            };
            let mut response = [0; HEADER.len() + 3];
            response[..HEADER.len()].copy_from_slice(HEADER);
            response[HEADER.len()..].copy_from_slice(&[
                u8::from(state.connected),
                encode_phase(state.outgoing),
                encode_phase(state.incoming),
            ]);
            stream.write_all(&response).await?;
            for progress in [state.sent, state.received] {
                stream.write_u64(progress.bytes).await?;
                stream.write_u64(progress.total).await?;
            }
        }
        tray::STATUS => {
            stream.write_all(HEADER).await?;
            tray::write_view(&mut stream, tray_view()).await?;
        }
        tray::COMMAND => {
            let command = tray::Command::read(&mut stream).await?;
            stream.write_all(HEADER).await?;
            stream.write_u8(u8::from(tray_command(command))).await?;
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
        ClipboardPhase, ClipboardStatus, Status, query, query_clipboard, query_clipboard_legacy,
        respond, respond_with_clipboard,
    };

    #[tokio::test]
    async fn clipboard_round_trip_with_partial_reads_and_writes() {
        let (mut client, mut server) = duplex(5);
        let expected = ClipboardStatus {
            connected: true,
            outgoing: ClipboardPhase::Offered,
            incoming: ClipboardPhase::Ready,
            ..ClipboardStatus::default()
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
    async fn legacy_monitor_can_read_status_while_a_file_is_offered() {
        let (mut client, mut server) = duplex(5);
        let offered = ClipboardStatus {
            connected: true,
            outgoing: ClipboardPhase::Offered,
            incoming: ClipboardPhase::Ready,
            ..ClipboardStatus::default()
        };
        let (reply, sent) = tokio::join!(
            query_clipboard_legacy(&mut client),
            respond_with_clipboard(
                &mut server,
                || panic!("unexpected status request"),
                || offered
            ),
        );
        assert_eq!(
            reply.unwrap(),
            ClipboardStatus {
                outgoing: ClipboardPhase::Waiting,
                ..offered
            }
        );
        sent.unwrap();
    }

    #[tokio::test]
    async fn approval_is_queried_and_decided_by_offer_identifier() {
        use super::{Approval, ApprovalDecision, approval, decide, respond_with_transfers};

        let (mut client, mut server) = duplex(8);
        let (offer, served) = tokio::join!(
            approval(&mut client, 42),
            respond_with_transfers(
                &mut server,
                || panic!("expected approval query"),
                || panic!("expected approval query"),
                |id| (id == 42).then(|| Approval {
                    id,
                    bytes: 6_000_000,
                    label: "sample.bin".into()
                }),
                |_| panic!("expected approval query")
            ),
        );
        served.unwrap();
        let offer = offer.unwrap().unwrap();
        assert_eq!(
            (offer.id, offer.bytes, offer.label.as_str()),
            (42, 6_000_000, "sample.bin")
        );
        for (id, expected) in [(41, false), (42, true)] {
            let (mut client, mut server) = duplex(8);
            let (decision, served) = tokio::join!(
                decide(&mut client, ApprovalDecision { id, accept: true }),
                respond_with_transfers(
                    &mut server,
                    || panic!("expected decision"),
                    || panic!("expected decision"),
                    |_| panic!("expected decision"),
                    |decision| decision.id == 42 && decision.accept
                ),
            );
            served.unwrap();
            assert_eq!(decision.unwrap(), expected);
        }
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
