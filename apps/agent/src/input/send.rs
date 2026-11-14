use std::{cell::Cell, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout, timeout_at};
use tracing::{info, warn};
use unikvm_core::focus::{ControlSession, FocusEvent, FocusState, SessionId};
use unikvm_core::geometry::Edge;
use unikvm_network::identity::Fingerprint;
use unikvm_platform::input::{Capture, Shortcut};
use unikvm_protocol::input::{
    self, EDGE_RETURN, Frame, GRANT, HEARTBEAT_ACK, HEARTBEAT_INTERVAL, HEARTBEAT_TOKENS, LEASE,
    REQUEST,
};

use super::Focus;

const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

pub async fn run(
    capture: &mut Capture,
    connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    state: &Cell<FocusState>,
    side: Option<Edge>,
) -> Result<()> {
    let mut requested = 0;

    while capture.shortcuts.changed().await.is_ok() {
        let shortcut = *capture.shortcuts.borrow_and_update();
        if shortcut.start == requested {
            continue;
        }
        requested = shortcut.start;
        if shortcut.cancelled {
            continue;
        }
        // Snapshot the connection. A reconnect requires a fresh shortcut or edge crossing.
        let connection = connections.borrow().clone();
        let Some(connection) = connection.filter(|connection| connection.close_reason().is_none())
        else {
            if shortcut.edge_height.is_none() {
                warn!("input peer is offline");
            }
            continue;
        };
        while capture.events.try_recv().is_ok() {}
        let session = ControlSession {
            peer: peer.device_id(),
            id: SessionId(shortcut.start),
        };
        let Some(focus) = Focus::reserve(state, session, FocusEvent::Request(session)) else {
            continue;
        };
        let result = control(capture, connection, &focus, session, shortcut, side).await;
        requested = capture.shortcuts.borrow_and_update().start;
        drop(focus);
        match result.as_ref() {
            Ok(Some(height)) => capture.return_from_edge(*height),
            _ => capture.local(),
        }
        if let Err(error) = result {
            warn!(%error, "input session ended");
        }
    }
    anyhow::bail!("local input monitor stopped")
}

async fn control(
    capture: &mut Capture,
    connection: quinn::Connection,
    focus: &Focus<'_>,
    session: ControlSession,
    shortcut: Shortcut,
    side: Option<Edge>,
) -> Result<Option<u16>> {
    let (mut send, mut recv, initial_heartbeat) = tokio::select! {
        biased;
        () = wait_stop(&mut capture.shortcuts, shortcut.stop) => return Ok(None),
        setup = timeout(LEASE, async {
            let (mut send, mut recv) = connection.open_bi().await?;
            let initial_heartbeat = if let Some((side, height)) = side.zip(shortcut.edge_height) {
                let sent = Instant::now();
                send.write_all(&input::edge_start(side, height)).await?;
                Some(sent)
            } else {
                send.write_all(REQUEST).await?;
                None
            };
            let mut reply = [0];
            recv.read_exact(&mut reply).await?;
            ensure!(reply[0] == GRANT, "peer did not grant this input session");
            Ok::<_, anyhow::Error>((send, recv, initial_heartbeat))
        }) => setup.context("request input grant timed out")??,
    };
    ensure!(
        focus.transition(FocusEvent::Grant(session)) == FocusState::Sending(session),
        "input grant no longer matches"
    );
    let result = active(
        capture,
        &mut send,
        &mut recv,
        session.id.0,
        shortcut,
        initial_heartbeat,
    )
    .await;
    if let Ok(Some(_)) = result {
        let _ = send.finish();
        let _ = recv.stop(0u32.into());
        info!("input returned across the display edge");
        return result;
    }
    capture.local();
    // A failed timed write may have left an incomplete frame. Reset the stream in that case.
    if result.is_ok() {
        if timeout(WRITE_TIMEOUT, input::write(&mut send, Frame::Release))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            let _ = send.finish();
        } else {
            let _ = send.reset(0u32.into());
        }
    } else {
        let _ = send.reset(0u32.into());
    }
    let _ = recv.stop(0u32.into());
    info!("input returned locally");
    result
}

async fn wait_stop(shortcuts: &mut watch::Receiver<Shortcut>, stop: u64) {
    let _ = shortcuts.wait_for(|shortcut| shortcut.stop != stop).await;
}

async fn active(
    capture: &mut Capture,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    generation: u64,
    shortcut: Shortcut,
    initial_heartbeat: Option<Instant>,
) -> Result<Option<u16>> {
    let stop = shortcut.stop;
    let mut heartbeat = interval_at(
        initial_heartbeat.map_or_else(Instant::now, |sent| sent + HEARTBEAT_INTERVAL),
        HEARTBEAT_INTERVAL,
    );
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let deadline = sleep_until(initial_heartbeat.unwrap_or_else(Instant::now) + LEASE);
    tokio::pin!(deadline);
    let mut sequence = u8::from(initial_heartbeat.is_some());
    let mut pending = [None; HEARTBEAT_TOKENS as usize];
    pending[0] = initial_heartbeat;
    let mut started = false;
    let mut peer_stopped = false;

    loop {
        let mut ack = [0];
        let frame = tokio::select! {
            biased;
            () = wait_stop(&mut capture.shortcuts, stop) => {
                return Ok(None);
            }
            () = &mut deadline => anyhow::bail!("peer input lease expired"),
            reply = recv.read(&mut ack) => {
                let Some(1) = reply? else { return Ok(None); };
                if ack[0] == EDGE_RETURN && shortcut.edge_height.is_some() {
                    let mut height = [0; 2];
                    tokio::select! {
                        biased;
                        () = wait_stop(&mut capture.shortcuts, stop) => return Ok(None),
                        result = timeout_at(deadline.deadline(), recv.read_exact(&mut height)) => result.context("read edge return height")??,
                    }
                    return Ok(Some(u16::from_be_bytes(height)));
                }
                ensure!((HEARTBEAT_ACK..HEARTBEAT_ACK + HEARTBEAT_TOKENS).contains(&ack[0]), "invalid input acknowledgment");
                let sent = pending[usize::from(ack[0] - HEARTBEAT_ACK)].take().context("unexpected peer input acknowledgment")?;
                let expires = sent + LEASE;
                ensure!(expires > Instant::now(), "peer input acknowledgment arrived after the lease expired");
                if !peer_stopped {
                    if expires > deadline.deadline() {
                        deadline.as_mut().reset(expires);
                    }
                    capture.renew_session(generation, expires.into_std(), shortcut.edge_height.is_some());
                    if !started {
                        info!("sending input to peer");
                        started = true;
                    }
                }
                continue;
            }
            _ = heartbeat.tick(), if !peer_stopped => {
                let token = sequence;
                sequence = (sequence + 1) % HEARTBEAT_TOKENS;
                pending[usize::from(token)] = Some(Instant::now());
                Frame::Heartbeat(token)
            },
            event = capture.events.recv(), if !peer_stopped => Frame::Event(event.context("local input capture stopped")?),
        };

        let written = tokio::select! {
            biased;
            () = wait_stop(&mut capture.shortcuts, stop) => {
                // The write could have started, so reset instead of attempting a release frame.
                let _ = send.reset(0u32.into());
                anyhow::bail!("input capture stopped during a stream write");
            }
            result = timeout(WRITE_TIMEOUT, input::write(send, frame)) => result.context("input stream stalled")?,
        };
        if let Err(error) = written {
            if shortcut.edge_height.is_some()
                && matches!(
                    error.get_ref().and_then(|source| source.downcast_ref()),
                    Some(quinn::WriteError::Stopped(_))
                )
            {
                // STOP_SENDING can arrive before the ordered return position.
                // Keep reading briefly without extending the capture lease.
                peer_stopped = true;
                let until = deadline.deadline().min(Instant::now() + WRITE_TIMEOUT);
                deadline.as_mut().reset(until);
            } else {
                return Err(error.into());
            }
        }
    }
}
