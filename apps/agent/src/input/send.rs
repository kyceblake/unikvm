use std::{cell::Cell, num::NonZeroU64, path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::sync::{broadcast, watch};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout, timeout_at};
use tracing::{info, warn};
use unikvm_core::focus::{ControlSession, FocusEvent, FocusState, SessionId};
use unikvm_core::geometry::Edge;
use unikvm_network::identity::Fingerprint;
use unikvm_platform::input::{Capture, Shortcut};
use unikvm_protocol::input::{
    self, DRAG_PREPARE, DRAG_RETURN, DRAG_RETURN_GRANTED, EDGE_RETURN, Frame, GRANT, HEARTBEAT_ACK,
    HEARTBEAT_INTERVAL, HEARTBEAT_TOKENS, LEASE, REQUEST,
};

use super::{Focus, timing::SendProbes};
use crate::desktop::{Drag, NativeDrops, OutboundDrag};

const WRITE_TIMEOUT: Duration = Duration::from_millis(500);
const DRAG_SETUP: Duration = Duration::from_secs(5);

pub(super) struct DragSender {
    pub starts: broadcast::Receiver<PathBuf>,
    pub offers: broadcast::Sender<OutboundDrag>,
    pub returns: Option<NativeDrops>,
}

#[derive(Clone, Copy)]
enum ControlEnd {
    Local,
    Edge(u16),
    NativeReturn,
}

struct SendOptions<'a> {
    side: Option<Edge>,
    drag: Option<(PathBuf, &'a broadcast::Sender<OutboundDrag>)>,
    returns: Option<&'a NativeDrops>,
    diagnostics: bool,
}

struct ReturnTarget<'a> {
    drops: &'a NativeDrops,
    side: Edge,
    peer: unikvm_core::device::DeviceId,
    connection: usize,
}

struct ActiveMode<'a> {
    native: bool,
    returns: Option<ReturnTarget<'a>>,
    diagnostics: bool,
}

pub async fn run(
    capture: &mut Capture,
    connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    state: &Cell<FocusState>,
    (side, mut drags): (Option<Edge>, Option<DragSender>),
    diagnostics: bool,
) -> Result<()> {
    let mut requested = 0;
    let mut generation = 0_u64;

    loop {
        let (shortcut, drag) = tokio::select! {
            changed = capture.shortcuts.changed() => {
                changed.context("local input monitor stopped")?;
                let shortcut = *capture.shortcuts.borrow_and_update();
                if shortcut.start == requested { continue; }
                requested = shortcut.start;
                (shortcut, None)
            }
            path = async {
                match drags.as_mut() {
                    Some(drag) => drag.starts.recv().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                let Some((path, height)) = path.zip(side.and_then(|side| capture.drag_height(side))) else { continue; };
                let mut shortcut = *capture.shortcuts.borrow_and_update();
                shortcut.edge_height = Some(height);
                shortcut.cancelled = false;
                let Some(drags) = drags.as_ref() else { continue; };
                (shortcut, Some((path, &drags.offers)))
            }
        };
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
        generation = generation.wrapping_add(1);
        let session = ControlSession {
            peer: peer.device_id(),
            id: SessionId(generation),
        };
        let Some(focus) = Focus::reserve(state, session, FocusEvent::Request(session)) else {
            continue;
        };
        let returns = drags.as_ref().and_then(|drag| drag.returns.as_ref());
        let result = control(
            capture,
            connection,
            &focus,
            session,
            shortcut,
            SendOptions {
                side,
                drag,
                returns,
                diagnostics,
            },
        )
        .await;
        requested = capture.shortcuts.borrow_and_update().start;
        if let Some(drags) = &mut drags {
            while drags.starts.try_recv().is_ok() {}
        }
        drop(focus);
        match result.as_ref() {
            Ok(ControlEnd::Edge(height)) => capture.return_from_edge(*height),
            Ok(ControlEnd::NativeReturn) => {}
            _ => capture.local(),
        }
        if let Err(error) = result {
            warn!(%error, "input session ended");
        }
    }
}

async fn control(
    capture: &mut Capture,
    connection: quinn::Connection,
    focus: &Focus<'_>,
    session: ControlSession,
    shortcut: Shortcut,
    options: SendOptions<'_>,
) -> Result<ControlEnd> {
    let SendOptions {
        side,
        drag,
        returns,
        diagnostics,
    } = options;
    let native = drag.is_some();
    let (mut send, mut recv, mut initial_heartbeat) = tokio::select! {
        biased;
        _ = capture.shortcuts.wait_for(|state| state.stop != shortcut.stop || (native && state.drag_epoch != shortcut.drag_epoch)) => return Ok(ControlEnd::Local),
        setup = timeout(if native { DRAG_SETUP } else { LEASE }, async {
            let (mut send, mut recv) = connection.open_bi().await?;
            let initial_heartbeat = if let Some((path, offers)) = drag {
                use ring::rand::{SecureRandom, SystemRandom};
                let mut random = [0; 8];
                SystemRandom::new().fill(&mut random).map_err(|_| anyhow::anyhow!("native drag ID unavailable"))?;
                let nonce = NonZeroU64::new(u64::from_ne_bytes(random)).context("native drag ID unavailable")?;
                let (side, height) = side.zip(shortcut.edge_height).context("native drag edge unavailable")?;
                send.write_all(&input::drag_start(side, height, nonce)).await?;
                let mut reply = [0];
                recv.read_exact(&mut reply).await?;
                ensure!(reply[0] == DRAG_PREPARE, "peer did not accept the native drag");
                offers.send(OutboundDrag { route: Drag { peer: session.peer, connection: connection.stable_id(), nonce }, path })
                    .map_err(|_| anyhow::anyhow!("file sharing is unavailable"))?;
                None
            } else if let Some((side, height)) = side.zip(shortcut.edge_height) {
                let sent = Instant::now();
                let packet = if returns.is_some() { input::return_edge_start(side, height) } else { input::edge_start(side, height) };
                send.write_all(&packet).await?;
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
    if native {
        let now = Instant::now();
        capture.renew_drag(session.id.0, (now + LEASE).into_std(), shortcut.drag_epoch);
        let ready = timeout(
            WRITE_TIMEOUT,
            capture.shortcuts.wait_for(|state| {
                state.stop != shortcut.stop || state.drag_generation == Some(session.id.0)
            }),
        )
        .await
        .context("local native drag did not start")??;
        ensure!(
            ready.stop == shortcut.stop && ready.drag_generation == Some(session.id.0),
            "local drag ended before handoff"
        );
        drop(ready);
        let now = Instant::now();
        timeout(WRITE_TIMEOUT, input::write(&mut send, Frame::Heartbeat(0))).await??;
        initial_heartbeat = Some(now);
    }
    let result = active(
        capture,
        &mut send,
        &mut recv,
        session.id.0,
        shortcut,
        initial_heartbeat,
        ActiveMode {
            native,
            diagnostics,
            returns: returns.zip(side).map(|(drops, side)| ReturnTarget {
                drops,
                side,
                peer: session.peer,
                connection: connection.stable_id(),
            }),
        },
    )
    .await;
    if matches!(result, Ok(ControlEnd::Edge(_) | ControlEnd::NativeReturn)) {
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
    mode: ActiveMode<'_>,
) -> Result<ControlEnd> {
    let ActiveMode {
        native,
        returns,
        diagnostics,
    } = mode;
    let mut probes = diagnostics.then(SendProbes::new);
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
    pending[0] = initial_heartbeat.map(|sent| (sent, None));
    let mut started = false;
    let mut peer_stopped = false;
    let mut returned: Option<ReturningDrag> = None;
    let mut return_poll = tokio::time::interval(Duration::from_millis(15));

    loop {
        let mut ack = [0];
        let frame = tokio::select! {
            biased;
            () = wait_stop(&mut capture.shortcuts, stop) => {
                return Ok(ControlEnd::Local);
            }
            () = &mut deadline => anyhow::bail!("peer input lease expired"),
            reply = recv.read(&mut ack) => {
                let Some(1) = reply? else { return Ok(ControlEnd::Local); };
                if ack[0] == EDGE_RETURN && shortcut.edge_height.is_some() {
                    let mut height = [0; 2];
                    tokio::select! {
                        biased;
                        () = wait_stop(&mut capture.shortcuts, stop) => return Ok(ControlEnd::Local),
                        result = timeout_at(deadline.deadline(), recv.read_exact(&mut height)) => result.context("read edge return height")??,
                    }
                    ensure!(returned.is_none(), "ordinary return during native drag handoff");
                    return Ok(ControlEnd::Edge(u16::from_be_bytes(height)));
                }
                if ack[0] == DRAG_RETURN {
                    ensure!(!native && returned.is_none(), "unexpected native drag return");
                    let ReturnTarget { drops, side, peer, connection } = returns.as_ref().context("native drag return is disabled")?;
                    let (side, peer, connection) = (*side, *peer, *connection);
                    ensure!(shortcut.edge_height.is_some() && capture.drag_return_ready(side), "physical drag return is no longer eligible");
                    let mut body = [0; 10];
                    timeout_at(deadline.deadline(), recv.read_exact(&mut body)).await.context("read native drag return")??;
                    let height = u16::from_be_bytes(body[..2].try_into().unwrap());
                    let nonce = NonZeroU64::new(u64::from_be_bytes(body[2..].try_into().unwrap())).context("invalid native drag return nonce")?;
                    let route = Drag { peer, connection, nonce };
                    ensure!(drops.reserve_return(route), "native drop slot is unavailable");
                    returned = Some(ReturningDrag::new((*drops).clone(), route, side, height));
                    timeout(WRITE_TIMEOUT, input::write(send, Frame::DragReturnPrepared(nonce))).await??;
                    continue;
                }
                if ack[0] == DRAG_RETURN_GRANTED {
                    let drag = returned.as_mut().context("unexpected native drag return grant")?;
                    ensure!(drag.ready && Instant::now() < drag.deadline && capture.drag_return_ready(drag.side), "native drag ended before return grant");
                    capture.return_drag(drag.height);
                    capture.wait_local((Instant::now() + WRITE_TIMEOUT).into_std()).await?;
                    ensure!(drag.drops.activate(drag.id.context("native drop offer missing")?), "native return offer expired");
                    drag.activated = true;
                    return Ok(ControlEnd::NativeReturn);
                }
                ensure!((HEARTBEAT_ACK..HEARTBEAT_ACK + HEARTBEAT_TOKENS).contains(&ack[0]), "invalid input acknowledgment");
                let (sent, sample) = pending[usize::from(ack[0] - HEARTBEAT_ACK)].take().context("unexpected peer input acknowledgment")?;
                let expires = sent + LEASE;
                ensure!(expires > Instant::now(), "peer input acknowledgment arrived after the lease expired");
                if let (Some(probes), Some(kind)) = (&mut probes, sample) {
                    probes.latencies.record(kind, sent);
                }
                if !peer_stopped {
                    if expires > deadline.deadline() {
                        deadline.as_mut().reset(expires);
                    }
                    if native { capture.renew_drag(generation, expires.into_std(), shortcut.drag_epoch); }
                    else { capture.renew_session(generation, expires.into_std(), shortcut.edge_height.is_some()); }
                    if !started {
                        info!("sending input to peer");
                        started = true;
                    }
                }
                continue;
            }
            _ = return_poll.tick(), if returned.is_some() => {
                let drag = returned.as_mut().unwrap();
                ensure!(Instant::now() < drag.deadline && capture.drag_return_ready(drag.side), "native drag return expired or cancelled");
                if !drag.ready && drag.prepare()? {
                    timeout(WRITE_TIMEOUT, input::write(send, Frame::DragReturnReady(drag.route.nonce))).await??;
                    drag.ready = true;
                }
                continue;
            }
            _ = heartbeat.tick(), if !peer_stopped => {
                let token = sequence;
                sequence = (sequence + 1) % HEARTBEAT_TOKENS;
                pending[usize::from(token)] = Some((Instant::now(), None));
                Frame::Heartbeat(token)
            },
            event = capture.events.recv(), if !peer_stopped => Frame::Event(event.context("local input capture stopped")?),
        };

        // An existing heartbeat immediately after a sampled event measures the
        // receiver's acknowledgment of that ordered input on any peer build.
        let probe = if let (Frame::Event(event), Some(probes)) = (frame, &mut probes) {
            let now = Instant::now();
            probes.sample(event, now).map(|kind| {
                let token = sequence;
                sequence = (sequence + 1) % HEARTBEAT_TOKENS;
                pending[usize::from(token)] = Some((now, Some(kind)));
                token
            })
        } else {
            None
        };
        let written = tokio::select! {
            biased;
            () = wait_stop(&mut capture.shortcuts, stop) => {
                // The write could have started, so reset instead of attempting a release frame.
                let _ = send.reset(0u32.into());
                anyhow::bail!("input capture stopped during a stream write");
            }
            result = timeout(WRITE_TIMEOUT, async {
                input::write(send, frame).await?;
                if let Some(token) = probe {
                    input::write(send, Frame::Heartbeat(token)).await?;
                }
                std::io::Result::Ok(())
            }) => result.context("input stream stalled")?,
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

/// A return to the physical mouse owner keeps the old capture alive until both
/// native desktops are ready. Dropping any incomplete handoff cancels its offer.
struct ReturningDrag {
    drops: NativeDrops,
    route: Drag,
    side: Edge,
    height: u16,
    deadline: Instant,
    id: Option<u64>,
    helper: Option<std::process::Child>,
    ready: bool,
    activated: bool,
}

impl ReturningDrag {
    fn new(drops: NativeDrops, route: Drag, side: Edge, height: u16) -> Self {
        Self {
            drops,
            route,
            side,
            height,
            deadline: Instant::now() + DRAG_SETUP,
            id: None,
            helper: None,
            ready: false,
            activated: false,
        }
    }

    fn prepare(&mut self) -> Result<bool> {
        if self.id.is_none()
            && let Some(id) = self.drops.offered(self.route)
        {
            let side = match self.side {
                Edge::Left => "--left",
                Edge::Right => "--right",
                _ => anyhow::bail!("invalid native drag return edge"),
            };
            self.helper = Some(crate::desktop::Helper::launch(&[
                "drag-receive",
                side,
                "--returned",
                &id.to_string(),
            ])?);
            self.id = Some(id);
        }
        if let Some(helper) = &mut self.helper {
            ensure!(helper.try_wait()?.is_none(), "native return helper exited");
        }
        Ok(self.id.is_some_and(|id| self.drops.armed(id)))
    }
}

impl Drop for ReturningDrag {
    fn drop(&mut self) {
        self.drops.end_drag(self.route, self.activated);
        if !self.activated
            && let Some(helper) = &mut self.helper
        {
            let _ = helper.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unikvm_core::device::DeviceId;
    use unikvm_protocol::clipboard::FileOffer;
    use unikvm_protocol::local::file_drop::{Command, Offer, Reply};

    #[test]
    fn abandoning_a_return_handoff_rejects_its_offer_and_frees_the_slot() {
        let drops = NativeDrops::default();
        let route = Drag {
            peer: DeviceId([7; 16]),
            connection: 19,
            nonce: NonZeroU64::MIN,
        };
        assert!(drops.reserve_return(route));
        let pending = ReturningDrag::new(drops.clone(), route, Edge::Left, 1234);
        assert!(drops.offer_drag(
            route,
            27,
            Offer {
                id: 5,
                file: FileOffer {
                    name: "return.txt".into(),
                    size: 100,
                },
            },
            std::time::Instant::now() + DRAG_SETUP,
        ));
        assert_eq!(drops.command(Command::Armed(5)), Reply::Recorded);
        drop(pending);
        assert!(!drops.expects(route));
        assert_eq!(drops.finished(27), Some(false));
        assert_eq!(drops.command(Command::Begin(5)), Reply::Unavailable);
        assert!(drops.reserve_return(Drag {
            connection: 20,
            ..route
        }));
    }
}
