use std::{cell::Cell, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;
use tokio::time::{Instant, timeout, timeout_at};
use tracing::{info, warn};
use unikvm_core::focus::{ControlSession, FocusEvent, FocusState, SessionId};
use unikvm_core::input::InputEvent;
use unikvm_network::identity::Fingerprint;
use unikvm_platform::input::{CaptureControl, EdgeLayout, InputSink, MotionPacer, Shortcut};
use unikvm_protocol::input::{
    self, DENY, EDGE_REQUEST, Frame, GRANT, HEARTBEAT_ACK, LEASE, REQUEST,
};

use super::Focus;

const ADMISSION_TIMEOUT: Duration = Duration::from_millis(750);

pub async fn run(
    control: CaptureControl,
    mut signals: watch::Receiver<Shortcut>,
    mut connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    state: &Cell<FocusState>,
    edge: Option<EdgeLayout>,
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
            let result = receive(
                &mut send,
                &mut recv,
                state,
                session,
                &mut signals,
                edge,
                &control,
            )
            .await;
            let _ = send.finish();
            let _ = recv.stop(0u32.into());
            if let Err(error) = result {
                warn!(%error, "incoming input session ended");
            }
        }
        control.show_idle_cursor();
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
    edge: Option<EdgeLayout>,
    control: &CaptureControl,
) -> Result<()> {
    let mut request = [0; REQUEST.len()];
    let admission = Instant::now() + ADMISSION_TIMEOUT;
    timeout_at(admission, recv.read_exact(&mut request))
        .await
        .context("input request timed out")??;
    let entry = match (&request, edge) {
        (REQUEST, _) => None,
        (EDGE_REQUEST, Some(edge)) => {
            let mut entry = [0; 3];
            timeout_at(admission, recv.read_exact(&mut entry))
                .await
                .context("edge request timed out")??;
            match input::edge_entry(entry) {
                Some((side, height)) if side == edge.side => Some((edge, height)),
                _ => {
                    deny(send).await;
                    return Ok(());
                }
            }
        }
        _ => {
            deny(send).await;
            return Ok(());
        }
    };
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

    control.show_idle_cursor();
    let result = tokio::select! {
        biased;
        local = wait_local(signals, baseline) => local.map(|()| (true, None)),
        result = async {
            timeout(ADMISSION_TIMEOUT, send.write_all(&[GRANT]))
                .await
                .context("input grant timed out")??;
            info!("receiving peer input");
            inject(&mut sink, send, recv, entry).await
        } => result.map(|height| (false, height)),
    };
    sink.release_all().context("release synthetic input")?;
    let (local, height) = result?;
    if let Some(height) = height {
        timeout(
            ADMISSION_TIMEOUT,
            send.write_all(&input::edge_return(height)),
        )
        .await
        .context("report return across display edge")??;
        control.hide_after_edge_return(baseline);
        info!("peer input returned across the display edge");
    } else if local {
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
    entry: Option<(EdgeLayout, u16)>,
) -> Result<Option<u16>> {
    let mut deadline = Instant::now() + LEASE;
    // Reposition only after granting focus and reading the first heartbeat.
    let mut pending_entry = entry;
    let edge = entry.map(|(edge, _)| edge);
    let mut motion = if entry.is_none() {
        pointer_pacer(sink)
    } else {
        None
    };
    loop {
        let read = timeout_at(deadline, input::read(recv));
        let frame = if let Some(pacer) = &mut motion {
            tokio::pin!(read);
            loop {
                // Keep the same read alive on frame ticks. Dropping a partial
                // read would lose bytes from the ordered input stream.
                tokio::select! {
                    biased;
                    step = pacer.next() => {
                        let [dx, dy] = step.context("pace pointer movement")?;
                        if (dx != 0 || dy != 0)
                            && let Some(height) = move_pointer(sink, edge, dx, dy)?
                        {
                            return Ok(Some(height));
                        }
                        tokio::task::yield_now().await;
                    }
                    result = &mut read => break result,
                }
            }
        } else {
            read.await
        };

        match frame.context("peer input lease expired")?? {
            Frame::Release => {
                if let Some(pacer) = &mut motion {
                    flush_motion(pacer, sink)?;
                }
                return Ok(None);
            }
            Frame::Event(event) => {
                ensure!(
                    pending_entry.is_none(),
                    "first heartbeat required before edge input"
                );
                if let Some(pacer) = &mut motion {
                    if let InputEvent::PointerMove { dx, dy } = event
                        && pacer.queue(dx, dy)
                    {
                        tokio::task::yield_now().await;
                        continue;
                    }
                    // Preserve pointer position before discrete input, including
                    // clicks, and drain it before falling back to immediate motion.
                    flush_motion(pacer, sink)?;
                }
                if let InputEvent::PointerMove { dx, dy } = event {
                    if let Some(height) = move_pointer(sink, edge, dx, dy)? {
                        return Ok(Some(height));
                    }
                } else {
                    sink.send(event)?;
                }
            }
            Frame::Heartbeat(token) => {
                ensure!(
                    InputSink::has_permission(),
                    "local input posting became unavailable"
                );
                let arriving = pending_entry.take();
                if let Some((edge, height)) = arriving {
                    sink.enter_edge(edge, height)?;
                }
                timeout_at(deadline, send.write_all(&[HEARTBEAT_ACK + token]))
                    .await
                    .context("input acknowledgment timed out")??;
                deadline = Instant::now() + LEASE;
                if arriving.is_some() {
                    motion = pointer_pacer(sink);
                }
            }
        }
        // Let the local-activity branch run even when the peer has buffered many frames.
        tokio::task::yield_now().await;
    }
}

fn pointer_pacer(sink: &InputSink) -> Option<MotionPacer> {
    match MotionPacer::new(sink) {
        Ok(motion) => {
            if let Some(period) = motion.display_period() {
                tracing::debug!(
                    refresh_hz = period.as_secs_f64().recip(),
                    "pointer pacing initialized"
                );
            }
            Some(motion)
        }
        Err(error) => {
            warn!(%error, "frame timer unavailable; injecting motion as it arrives");
            None
        }
    }
}

fn move_pointer(
    sink: &mut InputSink,
    edge: Option<EdgeLayout>,
    dx: i32,
    dy: i32,
) -> std::io::Result<Option<u16>> {
    if let Some(edge) = edge
        && let Some(height) = sink.exit_edge(edge, dx, dy)?
    {
        return Ok(Some(height));
    }
    sink.send(InputEvent::PointerMove { dx, dy })?;
    Ok(None)
}

fn flush_motion(pacer: &mut MotionPacer, sink: &mut InputSink) -> std::io::Result<()> {
    let [dx, dy] = pacer.take_pending();
    if dx != 0 || dy != 0 {
        sink.send(InputEvent::PointerMove { dx, dy })?;
    }
    Ok(())
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
