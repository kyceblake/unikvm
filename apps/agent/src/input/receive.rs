use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;
use tokio::time::{Instant, timeout, timeout_at};
use tracing::{info, warn};
use unikvm_core::focus::{ControlSession, FocusEvent, FocusState, SessionId};
use unikvm_network::identity::Fingerprint;
use unikvm_platform::input::MacInputSink;
use unikvm_protocol::input::{self, DENY, Frame, GRANT, HEARTBEAT_ACK, LEASE, REQUEST};

const ADMISSION_TIMEOUT: Duration = Duration::from_millis(750);

pub async fn run(
    mut connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
) -> Result<()> {
    if !MacInputSink::has_permission() {
        warn!("grant Accessibility to the Mac agent before requesting input control");
    }
    let mut session_id = 0u64;
    loop {
        let connection = match connections.wait_for(Option::is_some).await {
            Ok(snapshot) => snapshot
                .clone()
                .expect("wait_for returned a live connection"),
            Err(_) => break,
        };
        while let Ok((mut send, mut recv)) = connection.accept_bi().await {
            session_id = session_id.wrapping_add(1);
            let session = ControlSession {
                peer: peer.device_id(),
                id: SessionId(session_id),
            };
            let result = receive(&mut send, &mut recv, session).await;
            let _ = send.finish();
            let _ = recv.stop(0u32.into());
            if let Err(error) = result {
                warn!(%error, "Mac input session ended");
            }
        }
        // The close event may still be on its way through the transport.
        let closed = connection.stable_id();
        if connections
            .wait_for(|current| {
                current
                    .as_ref()
                    .is_none_or(|next| next.stable_id() != closed)
            })
            .await
            .is_err()
        {
            break;
        }
    }
    Ok(())
}

async fn receive(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    session: ControlSession,
) -> Result<()> {
    let mut request = [0; REQUEST.len()];
    timeout(ADMISSION_TIMEOUT, recv.read_exact(&mut request))
        .await
        .context("input request timed out")??;
    if request != *REQUEST {
        return Ok(());
    }
    let mut sink = match MacInputSink::new() {
        Ok(sink) => sink,
        Err(error) => {
            let _ = timeout(ADMISSION_TIMEOUT, send.write_all(&[DENY])).await;
            return Err(error.into());
        }
    };

    let state = FocusState::Local.transition(FocusEvent::AcceptIncoming(session));
    ensure!(state == FocusState::Receiving(session), "Mac input is busy");
    timeout(ADMISSION_TIMEOUT, send.write_all(&[GRANT]))
        .await
        .context("input grant timed out")??;
    info!("receiving Windows input");
    let result = inject(&mut sink, send, recv).await;
    let releases = sink.release_all().context("release synthetic input");
    info!("Mac input session closed");
    releases?;
    result
}

async fn inject(
    sink: &mut MacInputSink,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
) -> Result<()> {
    let mut deadline = Instant::now() + LEASE;
    loop {
        match timeout_at(deadline, input::read(recv))
            .await
            .context("Windows input lease expired")??
        {
            Frame::Release => return Ok(()),
            Frame::Event(event) => sink.send(event)?,
            Frame::Heartbeat(token) => {
                ensure!(
                    MacInputSink::has_permission(),
                    "Mac Accessibility permission was revoked"
                );
                timeout_at(deadline, send.write_all(&[HEARTBEAT_ACK + token]))
                    .await
                    .context("input acknowledgment timed out")??;
                deadline = Instant::now() + LEASE;
            }
        }
    }
}
