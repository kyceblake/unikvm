use std::{cell::Cell, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until, timeout};
use tracing::{info, warn};
use unikvm_core::focus::{ControlSession, FocusEvent, FocusState, SessionId};
use unikvm_network::identity::Fingerprint;
use unikvm_platform::input::{Capture, Shortcut};
use unikvm_protocol::input::{
    self, Frame, GRANT, HEARTBEAT_ACK, HEARTBEAT_INTERVAL, HEARTBEAT_TOKENS, LEASE, REQUEST,
};

use super::Focus;

const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

pub async fn run(
    capture: &mut Capture,
    connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    state: &Cell<FocusState>,
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
        // Snapshot the selected connection. Reconnects always require a new shortcut.
        let connection = connections.borrow().clone();
        let Some(connection) = connection.filter(|connection| connection.close_reason().is_none())
        else {
            warn!("input peer is offline");
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
        let result = control(capture, connection, &focus, session, shortcut.stop).await;
        capture.local();
        drop(focus);
        requested = capture.shortcuts.borrow_and_update().start;
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
    stop: u64,
) -> Result<()> {
    let (mut send, mut recv) = tokio::select! {
        biased;
        () = wait_stop(&mut capture.shortcuts, stop) => return Ok(()),
        setup = timeout(LEASE, async {
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(REQUEST).await?;
            let mut reply = [0];
            recv.read_exact(&mut reply).await?;
            ensure!(reply[0] == GRANT, "peer did not grant this input session");
            Ok::<_, anyhow::Error>((send, recv))
        }) => setup.context("request input grant timed out")??,
    };
    ensure!(
        focus.transition(FocusEvent::Grant(session)) == FocusState::Sending(session),
        "input grant no longer matches"
    );
    let result = active(capture, &mut send, &mut recv, session.id.0, stop).await;
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
    stop: u64,
) -> Result<()> {
    let mut heartbeat = interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let deadline = sleep_until(Instant::now() + LEASE);
    tokio::pin!(deadline);
    let mut sequence = 0u8;
    let mut pending = [None; HEARTBEAT_TOKENS as usize];
    let mut started = false;

    loop {
        let mut ack = [0];
        let frame = tokio::select! {
            biased;
            () = wait_stop(&mut capture.shortcuts, stop) => {
                return Ok(());
            }
            () = &mut deadline => anyhow::bail!("peer input lease expired"),
            reply = recv.read(&mut ack) => {
                let Some(1) = reply? else { return Ok(()); };
                ensure!((HEARTBEAT_ACK..HEARTBEAT_ACK + HEARTBEAT_TOKENS).contains(&ack[0]), "invalid input acknowledgment");
                let sent = pending[usize::from(ack[0] - HEARTBEAT_ACK)].take().context("unexpected peer input acknowledgment")?;
                let expires = sent + LEASE;
                ensure!(expires > Instant::now(), "peer input acknowledgment arrived after the lease expired");
                if expires > deadline.deadline() {
                    deadline.as_mut().reset(expires);
                }
                capture.renew(generation, expires.into_std());
                if !started {
                    info!("sending input to peer");
                    started = true;
                }
                continue;
            }
            _ = heartbeat.tick() => {
                let token = sequence;
                sequence = (sequence + 1) % HEARTBEAT_TOKENS;
                pending[usize::from(token)] = Some(Instant::now());
                Frame::Heartbeat(token)
            },
            event = capture.events.recv() => Frame::Event(event.context("local input capture stopped")?),
        };

        tokio::select! {
            biased;
            () = wait_stop(&mut capture.shortcuts, stop) => {
                // The write could have started, so reset instead of attempting a release frame.
                let _ = send.reset(0u32.into());
                anyhow::bail!("input capture stopped during a stream write");
            }
            result = timeout(WRITE_TIMEOUT, input::write(send, frame)) => result.context("input stream stalled")??,
        }
    }
}
