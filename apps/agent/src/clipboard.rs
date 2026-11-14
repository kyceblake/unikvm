use unikvm_config::ClipboardConfig;
use unikvm_network::PeerEndpoint;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn start(
    _: &ClipboardConfig,
    _: Option<&PeerEndpoint>,
    _: tokio::sync::watch::Sender<unikvm_protocol::local::ClipboardStatus>,
) -> anyhow::Result<()> {
    anyhow::bail!("clipboard sharing is available on Windows and macOS");
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use sync::start;

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod sync {
    use std::cell::Cell;
    use std::num::NonZeroU64;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context, Result, ensure};
    use tokio::sync::{mpsc, oneshot, watch};
    use tokio::time::{Instant, MissedTickBehavior, interval, timeout};
    use tracing::{debug, info, warn};
    use unikvm_clipboard::Revisions;
    use unikvm_core::clipboard::Content;
    use unikvm_core::device::DeviceId;
    use unikvm_platform::clipboard::{Clipboard, WriteOutcome};
    use unikvm_protocol::clipboard::{self, READY, Receipt, Record, Update};
    use unikvm_protocol::local::{ClipboardPhase as Phase, ClipboardStatus};

    use super::{ClipboardConfig, PeerEndpoint};

    const POLL: Duration = Duration::from_millis(150);
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);

    pub struct Worker(Option<oneshot::Sender<()>>);

    impl Drop for Worker {
        fn drop(&mut self) {
            if let Some(stop) = self.0.take() {
                let _ = stop.send(());
            }
        }
    }

    pub fn start(
        config: &ClipboardConfig,
        endpoint: Option<&PeerEndpoint>,
        status: watch::Sender<ClipboardStatus>,
    ) -> Result<Worker> {
        let peer = config.peer_fingerprint.parse()?;
        let endpoint = endpoint.context("clipboard sharing requires a network endpoint")?;
        let connections = endpoint
            .connection(peer)
            .context("clipboard peer must be configured in the network trust set")?;
        let local = endpoint.fingerprint().device_id();
        let (stop, stopped) = oneshot::channel();
        // Clipboard reads can wait for an application or an OS privacy prompt.
        // Own native objects on this thread and keep the input and network loops responsive.
        std::thread::Builder::new()
            .name("unikvm clipboard".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()?;
                    let clipboard = Clipboard::new().context("open native clipboard")?;
                    runtime.block_on(async {
                    tokio::select! {
                        _ = stopped => Ok(()),
                        result = run(connections, &clipboard, local, peer.device_id(), &status) => result,
                    }
                })
                })();
                status.send_modify(|state| state.connected = false);
                if let Err(error) = result {
                    warn!(%error, "clipboard sync stopped");
                }
            })?;
        Ok(Worker(Some(stop)))
    }

    async fn run(
        mut connections: watch::Receiver<Option<quinn::Connection>>,
        os: &Clipboard,
        local: DeviceId,
        peer: DeviceId,
        status: &watch::Sender<ClipboardStatus>,
    ) -> Result<()> {
        loop {
            let connection = connections
                .wait_for(Option::is_some)
                .await?
                .clone()
                .context("clipboard connection missing")?;
            if let Err(error) = session(&connection, os, local, peer, status).await {
                debug!(%error, "clipboard session ended");
            }
            status.send_modify(|state| {
                state.connected = false;
                for phase in [&mut state.outgoing, &mut state.incoming] {
                    *phase = match *phase {
                        Phase::Copying | Phase::Failed => Phase::Failed,
                        _ => Phase::Idle,
                    };
                }
            });
            connections
                .wait_for(|current| {
                    current
                        .as_ref()
                        .is_none_or(|next| next.stable_id() != connection.stable_id())
                })
                .await?;
        }
    }

    async fn session(
        connection: &quinn::Connection,
        os: &Clipboard,
        local: DeviceId,
        peer: DeviceId,
        status: &watch::Sender<ClipboardStatus>,
    ) -> Result<()> {
        let (send, mut recv) = timeout(HANDSHAKE_TIMEOUT, async {
            tokio::try_join!(
                async {
                    let mut send = connection.open_uni().await?;
                    send.write_all(READY).await?;
                    Ok::<_, anyhow::Error>(send)
                },
                async {
                    let mut recv = connection.accept_uni().await?;
                    let mut header = [0; READY.len()];
                    recv.read_exact(&mut header).await?;
                    ensure!(&header == READY, "unsupported clipboard session");
                    Ok::<_, anyhow::Error>(recv)
                }
            )
        })
        .await
        .context("clipboard peer did not enable sync")??;
        let (updates, outgoing) = watch::channel(None);
        let (receipts, to_write) = mpsc::channel(8);
        tokio::try_join!(
            send_updates(send, outgoing, to_write),
            reconcile(
                os,
                &mut recv,
                updates,
                Revisions::new(local, peer),
                peer,
                status,
                receipts,
            )
        )?;
        Ok(())
    }

    async fn send_updates(
        mut send: quinn::SendStream,
        mut updates: watch::Receiver<Option<Arc<Update>>>,
        mut receipts: mpsc::Receiver<Receipt>,
    ) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                receipt = receipts.recv() => {
                    let Some(receipt) = receipt else { return Ok(()); };
                    timeout(UPDATE_TIMEOUT, clipboard::write_receipt(&mut send, receipt))
                        .await.context("clipboard receipt write timed out")??;
                    continue;
                }
                changed = updates.changed() => {
                    if changed.is_err() { return Ok(()); }
                }
            }
            let update = updates.borrow_and_update().clone();
            if let Some(update) = update {
                timeout(UPDATE_TIMEOUT, clipboard::write(&mut send, &update))
                    .await
                    .context("clipboard update write timed out")??;
                if matches!(update.content(), Content::Png(_)) {
                    debug!("sent clipboard image");
                }
            }
        }
    }

    async fn reconcile(
        os: &Clipboard,
        recv: &mut quinn::RecvStream,
        outgoing: watch::Sender<Option<Arc<Update>>>,
        mut revisions: Revisions,
        peer: DeviceId,
        status: &watch::Sender<ClipboardStatus>,
        receipts: mpsc::Sender<Receipt>,
    ) -> Result<()> {
        let mut sequence = os.sequence()?;
        status.send_replace(ClipboardStatus {
            connected: true,
            ..ClipboardStatus::default()
        });
        info!("clipboard sync connected");
        let mut pending: Option<(Update, Instant)> = None;
        let mut awaiting_receipt: Option<(NonZeroU64, Instant)> = None;
        let mut preparing_until: Option<Instant> = None;
        let mut observed_sequence = sequence;
        let mut poll = interval(POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut was_available = os.available();
        loop {
            let receiving_until = Cell::new(None);
            let read = clipboard::read_record(recv, |revision| {
                if revision.origin == peer {
                    status.send_modify(|state| state.incoming = Phase::Copying);
                }
                receiving_until.set(Some(Instant::now() + UPDATE_TIMEOUT));
            });
            tokio::pin!(read);
            loop {
                let update = tokio::select! {
                    result = &mut read => Some(result?),
                    _ = poll.tick() => None,
                };
                let now = Instant::now();
                ensure!(
                    update.is_some() || receiving_until.get().is_none_or(|deadline| now < deadline),
                    "clipboard payload read timed out"
                );
                let received = update.is_some();
                let update = match update {
                    Some(Record::Receipt(receipt)) => {
                        if awaiting_receipt.is_some_and(|(counter, _)| counter == receipt.counter) {
                            awaiting_receipt = None;
                            status.send_modify(|state| {
                                state.outgoing = if receipt.applied {
                                    Phase::Ready
                                } else {
                                    Phase::Failed
                                };
                            });
                        }
                        None
                    }
                    Some(Record::Update(update)) => {
                        ensure!(
                            update.revision().origin == peer,
                            "invalid clipboard update origin"
                        );
                        Some(update)
                    }
                    None => None,
                };
                if awaiting_receipt.is_some_and(|(_, deadline)| now >= deadline) {
                    awaiting_receipt = None;
                    status.send_modify(|state| state.outgoing = Phase::Failed);
                }
                if pending
                    .as_ref()
                    .is_some_and(|(_, deadline)| now >= *deadline)
                    && let Some((old, _)) = pending.take()
                {
                    reply(&receipts, status, &old, false)?;
                    if receiving_until.get().is_some() {
                        status.send_modify(|state| state.incoming = Phase::Copying);
                    }
                }
                if !os.available() {
                    if was_available {
                        outgoing.send_replace(None);
                        awaiting_receipt = None;
                        preparing_until = None;
                        status.send_modify(|state| {
                            if state.outgoing == Phase::Copying {
                                state.outgoing = Phase::Failed;
                            }
                        });
                    }
                    was_available = false;
                    if let Some((update, _)) = pending.take() {
                        reply(&receipts, status, &update, false)?;
                    }
                    if let Some(update) = update {
                        reply(&receipts, status, &update, false)?;
                    }
                } else {
                    if !was_available {
                        sequence = os.sequence()?;
                        observed_sequence = sequence;
                        was_available = true;
                    }
                    if let Some(update) = update {
                        if !revisions.is_new_remote(&update)
                            || pending
                                .as_ref()
                                .is_some_and(|(old, _)| update.revision() <= old.revision())
                        {
                            reply(&receipts, status, &update, false)?;
                        } else {
                            if let Some((old, _)) = pending.take() {
                                reply(&receipts, status, &old, false)?;
                            }
                            status.send_modify(|state| state.incoming = Phase::Copying);
                            if matches!(update.content(), Content::Png(_)) {
                                debug!("received clipboard image");
                            }
                            pending = Some((update, now + UPDATE_TIMEOUT));
                        }
                    }
                    // Observe a local change before applying any peer update.
                    // Busy native clipboards are retried on the next tick.
                    if let Ok(observed) = os.sequence()
                        && observed != observed_sequence
                    {
                        observed_sequence = observed;
                        awaiting_receipt = None;
                        preparing_until = Some(now + UPDATE_TIMEOUT);
                        outgoing.send_replace(None);
                        status.send_modify(|state| {
                            state.outgoing = Phase::Copying;
                            if state.incoming == Phase::Ready {
                                state.incoming = Phase::Idle;
                            }
                        });
                    }
                    if preparing_until.is_some_and(|deadline| now >= deadline) {
                        preparing_until = None;
                        status.send_modify(|state| state.outgoing = Phase::Failed);
                    }
                    if let Ok(snapshot) = os.read(sequence) {
                        if let Some(snapshot) = snapshot {
                            sequence = snapshot.sequence;
                            observed_sequence = sequence;
                            preparing_until = None;
                            if !os.available() {
                                outgoing.send_replace(None);
                                awaiting_receipt = None;
                                status.send_modify(|state| state.outgoing = Phase::Skipped);
                                if let Some((update, _)) = pending.take() {
                                    reply(&receipts, status, &update, false)?;
                                }
                                was_available = false;
                            } else if let Some(content) = snapshot.content.and_then(valid_local) {
                                if matches!(&content, Content::Png(_)) {
                                    debug!("queued clipboard image");
                                }
                                let update = revisions.local_change(content)?;
                                awaiting_receipt = Some((
                                    update.revision().counter,
                                    Instant::now() + UPDATE_TIMEOUT * 2,
                                ));
                                status.send_modify(|state| state.outgoing = Phase::Copying);
                                outgoing.send_replace(Some(Arc::new(update)));
                            } else {
                                outgoing.send_replace(None);
                                awaiting_receipt = None;
                                status.send_modify(|state| state.outgoing = Phase::Skipped);
                                if let Some((update, _)) = pending.take() {
                                    reply(&receipts, status, &update, false)?;
                                }
                            }
                        }
                        if (received || receiving_until.get().is_none())
                            && let Some((update, _)) = pending.as_ref()
                        {
                            if !revisions.is_new_remote(update) {
                                reply(&receipts, status, update, false)?;
                                pending = None;
                            } else if os.available() {
                                match os.write(sequence, update.content()) {
                                    Ok(WriteOutcome::Applied(applied)) => {
                                        if matches!(update.content(), Content::Png(_)) {
                                            debug!("applied clipboard image");
                                        }
                                        sequence = applied;
                                        observed_sequence = applied;
                                        revisions.accept_remote(update);
                                        reply(&receipts, status, update, true)?;
                                        pending = None;
                                    }
                                    Ok(WriteOutcome::Unsupported) => {
                                        debug!(
                                            "clipboard image exceeded limits or failed validation"
                                        );
                                        reply(&receipts, status, update, false)?;
                                        pending = None;
                                    }
                                    Ok(WriteOutcome::Changed) | Err(_) => {}
                                }
                            }
                        }
                    }
                }
                // A poll must not cancel a partially read record.
                if received {
                    break;
                }
            }
        }
    }

    fn reply(
        receipts: &mpsc::Sender<Receipt>,
        status: &watch::Sender<ClipboardStatus>,
        update: &Update,
        applied: bool,
    ) -> Result<()> {
        status.send_modify(|state| {
            state.incoming = if applied { Phase::Ready } else { Phase::Failed }
        });
        receipts
            .try_send(Receipt {
                counter: update.revision().counter,
                applied,
            })
            .context("clipboard receipt queue is full or closed")?;
        Ok(())
    }

    fn valid_local(content: Content) -> Option<Content> {
        match content {
            Content::Text { plain, html } => (!plain.contains('\0')).then(|| Content::Text {
                plain,
                html: html.filter(|html| !html.contains('\0')),
            }),
            Content::Png(_) => Some(content),
        }
    }
}
