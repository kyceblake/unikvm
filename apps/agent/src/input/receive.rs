use std::{cell::Cell, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;
use tokio::time::{Instant, timeout, timeout_at};
use tracing::{info, warn};
use unikvm_core::focus::{ControlSession, FocusEvent, FocusState, SessionId};
use unikvm_core::input::{ButtonState, InputEvent, PointerButton};
use unikvm_network::identity::Fingerprint;
use unikvm_platform::input::{CaptureControl, EdgeLayout, InputSink, MotionPacer, Shortcut};
use unikvm_protocol::input::{
    self, DENY, DRAG_PREPARE, DRAG_REQUEST, EDGE_REQUEST, Frame, GRANT, HEARTBEAT_ACK, LEASE,
    REQUEST,
};

use super::Focus;

const ADMISSION_TIMEOUT: Duration = Duration::from_millis(750);
const DRAG_SETUP: Duration = Duration::from_secs(4);
const DRAG_BUTTON_DOWN: InputEvent = InputEvent::PointerButton {
    button: PointerButton::Primary,
    state: ButtonState::Pressed,
};
const DRAG_BUTTON_UP: InputEvent = InputEvent::PointerButton {
    button: PointerButton::Primary,
    state: ButtonState::Released,
};

pub(super) struct Options {
    pub edge: Option<EdgeLayout>,
    pub allow_locked_receive: bool,
    pub native: Option<crate::desktop::NativeDrops>,
}

pub async fn run(
    control: CaptureControl,
    mut signals: watch::Receiver<Shortcut>,
    mut connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    state: &Cell<FocusState>,
    options: Options,
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
                (session, connection.stable_id()),
                &mut signals,
                &options,
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
    (session, connection): (ControlSession, usize),
    signals: &mut watch::Receiver<Shortcut>,
    options: &Options,
    control: &CaptureControl,
) -> Result<()> {
    let mut request = [0; REQUEST.len()];
    let admission = Instant::now() + ADMISSION_TIMEOUT;
    timeout_at(admission, recv.read_exact(&mut request))
        .await
        .context("input request timed out")??;
    let mut route = None;
    let entry = match (&request, options.edge) {
        (REQUEST, _) => None,
        (EDGE_REQUEST | DRAG_REQUEST, Some(edge)) => {
            if &request == DRAG_REQUEST && options.native.is_none() {
                deny(send).await;
                return Ok(());
            }
            let mut entry = [0; 3];
            timeout_at(admission, recv.read_exact(&mut entry))
                .await
                .context("edge request timed out")??;
            match input::edge_entry(entry) {
                Some((side, height)) if side == edge.side => {
                    if &request == DRAG_REQUEST {
                        use tokio::io::AsyncReadExt;
                        let nonce = timeout_at(admission, recv.read_u64()).await??;
                        let Some(nonce) = std::num::NonZeroU64::new(nonce) else {
                            deny(send).await;
                            return Ok(());
                        };
                        route = Some(crate::desktop::Drag {
                            peer: session.peer,
                            connection,
                            nonce,
                        });
                    }
                    Some((edge, height))
                }
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
    let mut sink = match InputSink::with_locked_receive(options.allow_locked_receive) {
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

    let edge_session = entry.is_some();
    control.show_idle_cursor();
    let mut drag = match route {
        Some(route) => {
            let drops = options
                .native
                .as_ref()
                .context("native delivery is disabled")?;
            if !drops.reserve(route) {
                deny(send).await;
                return Ok(());
            }
            Some(ReceivingDrag {
                drops,
                route,
                id: None,
                helper: None,
                released: false,
            })
        }
        None => None,
    };
    let result = tokio::select! {
        biased;
        local = wait_local(signals, baseline) => local.map(|()| (true, None)),
        result = async {
            if let (Some(drag), Some((edge, _))) = (&mut drag, entry) {
                timeout(DRAG_SETUP, drag.prepare(send, edge.side)).await.context("native drop helper timed out")??;
            }
            timeout(ADMISSION_TIMEOUT, send.write_all(&[GRANT]))
                .await
                .context("input grant timed out")??;
            info!("receiving peer input");
            inject(&mut sink, send, recv, entry, options.edge, &mut drag).await
        } => result.map(|height| (false, height)),
    };
    drop(drag);
    sink.release_all().context("release synthetic input")?;
    let (local, height) = result?;
    if let Some(height) = height {
        // Hide at the completed crossing before the network write can stall or
        // the sender resumes its pointer. Failed returns must reveal it again.
        control.hide_after_edge_return(baseline);
        let reported = report_return(send, edge_session, height).await;
        if let Err(error) = reported {
            control.show_idle_cursor();
            return Err(error);
        }
        info!("peer input returned across the display edge");
    } else if local {
        focus.transition(FocusEvent::LocalActivity);
        info!("physical input reclaimed local control");
    } else {
        info!("incoming input session closed");
    }
    Ok(())
}

struct ReceivingDrag<'a> {
    drops: &'a crate::desktop::NativeDrops,
    route: crate::desktop::Drag,
    id: Option<u64>,
    helper: Option<std::process::Child>,
    released: bool,
}

impl ReceivingDrag<'_> {
    async fn prepare(
        &mut self,
        send: &mut quinn::SendStream,
        side: unikvm_core::geometry::Edge,
    ) -> Result<()> {
        send.write_all(&[DRAG_PREPARE]).await?;
        let id = loop {
            if let Some(id) = self.drops.offered(self.route) {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        };
        self.id = Some(id);
        let side = match side {
            unikvm_core::geometry::Edge::Left => "--left",
            unikvm_core::geometry::Edge::Right => "--right",
            _ => anyhow::bail!("invalid native drop edge"),
        };
        let helper = self.helper.insert(crate::desktop::Helper::launch(&[
            "drag-receive",
            side,
            "--granted",
            &id.to_string(),
        ])?);
        loop {
            if helper.try_wait()?.is_some() {
                anyhow::bail!("native drop helper exited");
            }
            if self.drops.armed(id) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    fn released_button(&mut self, sink: &mut InputSink) -> Result<()> {
        ensure!(
            self.id.is_some_and(|id| self.drops.release(id)),
            "native drop offer expired"
        );
        sink.send(DRAG_BUTTON_UP)?;
        self.released = true;
        Ok(())
    }
}

impl Drop for ReceivingDrag<'_> {
    fn drop(&mut self) {
        self.drops.end_drag(self.route, self.released);
        if !self.released
            && let Some(helper) = &mut self.helper
        {
            let _ = helper.kill();
        }
    }
}

async fn deny(send: &mut quinn::SendStream) {
    let _ = timeout(ADMISSION_TIMEOUT, send.write_all(&[DENY])).await;
}

async fn report_return(
    send: &mut (impl AsyncWrite + Unpin),
    edge_session: bool,
    height: u16,
) -> Result<()> {
    // Legacy shortcut senders accept EOF as release, but reject EDGE_RETURN.
    // The caller finishes the stream after this function returns.
    if edge_session {
        timeout(
            ADMISSION_TIMEOUT,
            send.write_all(&input::edge_return(height)),
        )
        .await
        .context("report return across display edge")??;
    }
    Ok(())
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
    return_edge: Option<EdgeLayout>,
    drag: &mut Option<ReceivingDrag<'_>>,
) -> Result<Option<u16>> {
    let mut deadline = Instant::now() + LEASE;
    // Reposition only after granting focus and reading the first heartbeat.
    let mut pending_entry = entry;
    let edge = entry.map(|(edge, _)| edge).or(return_edge);
    let mut was_locked = sink.is_locked_receiver();
    let mut motion = if entry.is_none() {
        pointer_pacer(sink)
    } else {
        None
    };
    loop {
        let locked = sink.is_locked_receiver();
        if was_locked && !locked {
            motion = pointer_pacer(sink);
        }
        was_locked = locked;
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
                    if let Some(drag) = drag.as_mut().filter(|drag| !drag.released)
                        && event == DRAG_BUTTON_UP
                    {
                        drag.released_button(sink)?;
                    } else {
                        sink.send(event)?;
                    }
                }
            }
            Frame::Heartbeat(token) => {
                if let Some(drag) = drag.as_mut().filter(|drag| !drag.released) {
                    ensure!(
                        drag.id.is_some_and(|id| drag.drops.armed(id)),
                        "native drag ended before release"
                    );
                }
                ensure!(
                    sink.check_permission(),
                    "local input posting became unavailable"
                );
                let arriving = pending_entry.take();
                if let Some((edge, height)) = arriving
                    && !sink.is_locked_receiver()
                {
                    sink.enter_edge(edge, height)?;
                    if drag.is_some() {
                        sink.send(DRAG_BUTTON_DOWN)?;
                    }
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
    if sink.is_locked_receiver() {
        return None;
    }
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
    if !sink.is_locked_receiver()
        && let Some(edge) = edge
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
    use tokio::io::{AsyncReadExt, duplex};

    #[tokio::test]
    async fn shortcut_return_uses_eof_instead_of_an_unexpected_edge_frame() {
        let (mut send, mut recv) = duplex(32);
        report_return(&mut send, false, 32768).await.unwrap();
        drop(send);
        let mut bytes = Vec::new();
        recv.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn edge_return_reports_the_mapped_height() {
        let (mut send, mut recv) = duplex(32);
        report_return(&mut send, true, 12345).await.unwrap();
        drop(send);
        let mut bytes = Vec::new();
        recv.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, input::edge_return(12345));
    }

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
