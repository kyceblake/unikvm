use std::{cell::Cell, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;
use tokio::time::{Instant, timeout, timeout_at};
use tracing::{info, warn};
use unikvm_core::focus::{ControlSession, FocusEvent, FocusState, SessionId};
use unikvm_network::identity::Fingerprint;
use unikvm_platform::input::{InputSink, Shortcut};
use unikvm_protocol::input::{self, DENY, Frame, GRANT, HEARTBEAT_ACK, LEASE, REQUEST};

use super::Focus;

const ADMISSION_TIMEOUT: Duration = Duration::from_millis(750);

pub async fn run(
    mut signals: watch::Receiver<Shortcut>,
    mut connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    state: &Cell<FocusState>,
) -> Result<()> {
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
            let result = receive(&mut send, &mut recv, state, session, &mut signals).await;
            let _ = send.finish();
            let _ = recv.stop(0u32.into());
            if let Err(error) = result {
                warn!(%error, "incoming input session ended");
            }
        }
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
    state: &Cell<FocusState>,
    session: ControlSession,
    signals: &mut watch::Receiver<Shortcut>,
) -> Result<()> {
    let mut request = [0; REQUEST.len()];
    timeout(ADMISSION_TIMEOUT, recv.read_exact(&mut request))
        .await
        .context("input request timed out")??;
    if request != *REQUEST {
        return Ok(());
    }
    let Some(focus) = Focus::reserve(state, session, FocusEvent::AcceptIncoming(session)) else {
        deny(send).await;
        return Ok(());
    };
    let baseline = signals.borrow().activity;
    let mut sink = match InputSink::new() {
        Ok(sink) if signals.has_changed().is_ok() => sink,
        Ok(_) => {
            deny(send).await;
            anyhow::bail!("local input monitor stopped");
        }
        Err(error) => {
            deny(send).await;
            return Err(error.into());
        }
    };

    let result = tokio::select! {
        biased;
        local = wait_local(signals, baseline) => local.map(|()| true),
        result = async {
            timeout(ADMISSION_TIMEOUT, send.write_all(&[GRANT]))
                .await
                .context("input grant timed out")??;
            info!("receiving peer input");
            inject(&mut sink, send, recv).await
        } => result.map(|()| false),
    };
    sink.release_all().context("release synthetic input")?;
    if result? {
        focus.transition(FocusEvent::LocalActivity);
        info!("physical input reclaimed local control");
    } else {
        info!("incoming input session closed");
    }
    Ok(())
}

async fn deny(send: &mut quinn::SendStream) {
    let _ = timeout(ADMISSION_TIMEOUT, send.write_all(&[DENY])).await;
}

async fn wait_local(signals: &mut watch::Receiver<Shortcut>, baseline: u64) -> Result<()> {
    signals
        .wait_for(|signal| signal.activity != baseline)
        .await
        .context("local input monitor stopped")?;
    Ok(())
}

async fn inject(
    sink: &mut InputSink,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
) -> Result<()> {
    let mut deadline = Instant::now() + LEASE;
    loop {
        match timeout_at(deadline, input::read(recv))
            .await
            .context("peer input lease expired")??
        {
            Frame::Release => return Ok(()),
            Frame::Event(event) => sink.send(event)?,
            Frame::Heartbeat(token) => {
                ensure!(
                    InputSink::has_permission(),
                    "local input posting became unavailable"
                );
                timeout_at(deadline, send.write_all(&[HEARTBEAT_ACK + token]))
                    .await
                    .context("input acknowledgment timed out")??;
                deadline = Instant::now() + LEASE;
            }
        }
        // Let the local-activity branch run even when the peer has buffered many frames.
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn takeover_only_responds_to_activity_after_the_session_snapshot() {
        let (native, mut signals) = watch::channel(Shortcut {
            activity: 1,
            ..Shortcut::default()
        });
        let baseline = signals.borrow().activity;
        native.send_modify(|signal| signal.start += 1);
        assert!(
            timeout(Duration::from_millis(5), wait_local(&mut signals, baseline))
                .await
                .is_err()
        );
        native.send_modify(|signal| signal.activity += 1);
        wait_local(&mut signals, baseline).await.unwrap();
    }
}
