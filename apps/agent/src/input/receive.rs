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
    self, DENY, DRAG_PREPARE, DRAG_REQUEST, DRAG_RETURN_GRANTED, EDGE_REQUEST, Frame, GRANT,
    HEARTBEAT_ACK, LEASE, REQUEST, RETURN_EDGE_REQUEST,
};

use super::{
    Focus,
    timing::{InputKind, Latencies, ReceivedSample},
};

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
    pub remote_drag: Option<crate::desktop::Desktop>,
    pub diagnostics: bool,
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
        (EDGE_REQUEST | RETURN_EDGE_REQUEST | DRAG_REQUEST, Some(edge)) => {
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
    let mut native_return = false;
    let remote_drag = (&request == RETURN_EDGE_REQUEST)
        .then_some(options.remote_drag.as_ref())
        .flatten();
    let mut returns = remote_drag.map(|desktop| ReturnSource {
        starts: desktop.drag_starts.subscribe(),
        offers: desktop.drag_offers.clone(),
        session,
        connection,
    });
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
            inject(&mut sink, send, recv, entry, options, &mut drag, (&mut returns, &mut native_return)).await
        } => result.map(|height| (false, height)),
    };
    drop(drag);
    sink.release_all().context("release synthetic input")?;
    let (local, height) = result?;
    if let Some(height) = height {
        // Hide at the completed crossing before the network write can stall or
        // the sender resumes its pointer. Failed returns must reveal it again.
        control.hide_after_edge_return(baseline);
        let reported = if native_return {
            Ok(())
        } else {
            report_return(send, edge_session, height).await
        };
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
    options: &Options,
    drag: &mut Option<ReceivingDrag<'_>>,
    (returns, native_return): (&mut Option<ReturnSource>, &mut bool),
) -> Result<Option<u16>> {
    let mut deadline = Instant::now() + LEASE;
    let mut pending_return = None;
    // Reposition only after granting focus and reading the first heartbeat.
    let mut pending_entry = entry;
    let edge = entry.map(|(edge, _)| edge).or(options.edge);
    let mut timings = options
        .diagnostics
        .then(|| Latencies::new("receive_to_native_post"));
    #[cfg(target_os = "macos")]
    sink.set_post_diagnostics(options.diagnostics);
    let mut motion_received: Option<ReceivedSample> = None;
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
            motion_received = None;
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
                    path = wait_return_source(returns) => {
                        pending_return = path;
                    }
                    step = pacer.next() => {
                        let ([dx, dy], late_by) = step.context("pace pointer movement")?;
                        let empty = dx == 0 && dy == 0;
                        if let Some(sample) = &mut motion_received {
                            sample.paced_step(late_by, empty);
                        }
                        if !empty {
                            let posting = motion_received.as_ref().map(|_| Instant::now());
                            if let Some(height) = sink.move_or_exit(edge, dx, dy)? {
                                return Ok(Some(height));
                            }
                            record_post(&mut timings, sink, InputKind::MouseMotion, motion_received.take().zip(posting));
                        }
                        tokio::task::yield_now().await;
                    }
                    result = &mut read => break result,
                }
            }
        } else {
            tokio::pin!(read);
            tokio::select! {
                result = &mut read => result,
                path = wait_return_source(returns) => {
                    pending_return = path;
                    read.await
                }
            }
        };

        match frame.context("peer input lease expired")?? {
            Frame::Release => {
                if let Some(pacer) = &mut motion {
                    flush_motion(pacer, sink, &mut timings, motion_received.take())?;
                }
                return Ok(None);
            }
            Frame::Event(event) => {
                ensure!(
                    pending_entry.is_none(),
                    "first heartbeat required before edge input"
                );
                let received = timings.as_ref().map(|_| ReceivedSample::new());
                if let Some(pacer) = &mut motion {
                    if let InputEvent::PointerMove { dx, dy } = event
                        && pacer.queue(dx, dy)
                    {
                        if dx != 0 || dy != 0 {
                            // Several motion frames can contribute to the same post.
                            motion_received = received;
                        }
                        tokio::task::yield_now().await;
                        continue;
                    }
                    // Preserve pointer position before discrete input, including
                    // clicks, and drain it before falling back to immediate motion.
                    flush_motion(pacer, sink, &mut timings, motion_received.take())?;
                }
                let posting = received.as_ref().map(|_| Instant::now());
                if let InputEvent::PointerMove { dx, dy } = event {
                    if let Some(height) = sink.move_or_exit(edge, dx, dy)? {
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
                record_post(&mut timings, sink, event.into(), received.zip(posting));
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
            Frame::DragReturnPrepared(_) | Frame::DragReturnReady(_) => {
                anyhow::bail!("unexpected drag return coordination frame");
            }
        }
        if let (Some(path), Some(edge), Some(source)) =
            (pending_return.take(), edge, returns.as_ref())
            && let Some(height) = sink.remote_drag_height(edge)
        {
            finish_remote_drag(sink, send, recv, path, (edge, height), source).await?;
            *native_return = true;
            return Ok(Some(height));
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

fn record_post(
    timings: &mut Option<Latencies>,
    sink: &mut InputSink,
    kind: InputKind,
    sample: Option<(ReceivedSample, Instant)>,
) {
    if let (Some(timings), Some((sample, posting))) = (timings, sample) {
        #[cfg(target_os = "macos")]
        let native = sink.take_post_timing();
        #[cfg(target_os = "windows")]
        let native = {
            let _ = sink;
            None
        };
        timings.posted(kind, sample, posting, native);
    }
}

fn flush_motion(
    pacer: &mut MotionPacer,
    sink: &mut InputSink,
    timings: &mut Option<Latencies>,
    sample: Option<ReceivedSample>,
) -> std::io::Result<()> {
    let [dx, dy] = pacer.take_pending();
    if dx != 0 || dy != 0 {
        let posting = sample.as_ref().map(|_| Instant::now());
        sink.send(InputEvent::PointerMove { dx, dy })?;
        record_post(timings, sink, InputKind::MouseMotion, sample.zip(posting));
    }
    Ok(())
}

struct ReturnSource {
    starts: tokio::sync::broadcast::Receiver<std::path::PathBuf>,
    offers: tokio::sync::broadcast::Sender<crate::desktop::OutboundDrag>,
    session: ControlSession,
    connection: usize,
}

async fn wait_return_source(source: &mut Option<ReturnSource>) -> Option<std::path::PathBuf> {
    match source {
        Some(source) => loop {
            match source.starts.recv().await {
                Ok(path) => return Some(path),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return std::future::pending().await,
            }
        },
        None => std::future::pending().await,
    }
}

async fn finish_remote_drag(
    sink: &mut InputSink,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    path: std::path::PathBuf,
    (edge, height): (EdgeLayout, u16),
    source: &ReturnSource,
) -> Result<()> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0; 8];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("native drag return nonce unavailable"))?;
    let nonce = std::num::NonZeroU64::new(u64::from_ne_bytes(bytes))
        .context("native drag return nonce unavailable")?;
    let deadline = Instant::now() + DRAG_SETUP;
    timeout_at(deadline, send.write_all(&input::drag_return(height, nonce))).await??;
    wait_return_stage(sink, send, recv, Frame::DragReturnPrepared(nonce), deadline).await?;
    source
        .offers
        .send(crate::desktop::OutboundDrag {
            route: crate::desktop::Drag {
                peer: source.session.peer,
                connection: source.connection,
                nonce,
            },
            path,
        })
        .map_err(|_| anyhow::anyhow!("file sharing unavailable during native drag return"))?;
    wait_return_stage(sink, send, recv, Frame::DragReturnReady(nonce), deadline).await?;
    ensure!(
        sink.remote_drag_height(edge).is_some(),
        "remote drag left the edge during setup"
    );
    sink.cancel_remote_drag()?;
    timeout_at(deadline, send.write_all(&[DRAG_RETURN_GRANTED])).await??;
    info!("native drag returned to physical mouse owner");
    Ok(())
}

async fn wait_return_stage(
    sink: &mut InputSink,
    send: &mut (impl AsyncWrite + Unpin),
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
    expected: Frame,
    deadline: Instant,
) -> Result<()> {
    loop {
        let frame = timeout_at(deadline, input::read(recv))
            .await
            .context("native drag return setup timed out")??;
        if return_stage_ready(frame, expected)? {
            ensure!(
                sink.check_permission(),
                "remote drag posting permission lost"
            );
            return Ok(());
        }
        match frame {
            Frame::Heartbeat(token) => {
                ensure!(
                    sink.check_permission(),
                    "remote drag posting permission lost"
                );
                timeout_at(deadline, send.write_all(&[HEARTBEAT_ACK + token])).await??;
            }
            Frame::Event(event @ InputEvent::PointerMove { .. }) => sink.send(event)?,
            Frame::Event(event) => {
                sink.send(event)?;
                anyhow::bail!("remote drag changed during return setup");
            }
            _ => anyhow::bail!("remote drag return cancelled or mismatched"),
        }
    }
}

fn return_stage_ready(frame: Frame, expected: Frame) -> Result<bool> {
    if frame == expected {
        return Ok(true);
    }
    ensure!(
        matches!(
            frame,
            Frame::Heartbeat(_) | Frame::Event(InputEvent::PointerMove { .. })
        ),
        "remote drag return cancelled or mismatched"
    );
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, duplex};

    #[test]
    fn return_setup_is_nonce_bound_and_release_or_escape_cancels() {
        let nonce = std::num::NonZeroU64::new(7).unwrap();
        let expected = Frame::DragReturnReady(nonce);
        assert!(return_stage_ready(expected, expected).unwrap());
        assert!(!return_stage_ready(Frame::Heartbeat(0), expected).unwrap());
        assert!(
            !return_stage_ready(
                Frame::Event(InputEvent::PointerMove { dx: 2, dy: 1 }),
                expected
            )
            .unwrap()
        );
        for frame in [
            Frame::DragReturnReady(std::num::NonZeroU64::new(8).unwrap()),
            Frame::DragReturnPrepared(nonce),
            Frame::Release,
            Frame::Event(DRAG_BUTTON_UP),
            Frame::Event(InputEvent::Key {
                usage: unikvm_core::input::KeyUsage(0x29),
                state: ButtonState::Pressed,
            }),
        ] {
            assert!(return_stage_ready(frame, expected).is_err());
        }
    }

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
