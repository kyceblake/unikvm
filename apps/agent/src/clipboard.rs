use unikvm_config::ClipboardConfig;
use unikvm_network::PeerEndpoint;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn start(_: &ClipboardConfig, _: Option<&PeerEndpoint>) -> anyhow::Result<()> {
    anyhow::bail!("clipboard sharing is available on Windows and macOS");
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use sync::start;

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod sync {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context, Result, ensure};
    use tokio::sync::{oneshot, watch};
    use tokio::time::{MissedTickBehavior, interval, timeout};
    use tracing::{debug, info, warn};
    use unikvm_clipboard::Revisions;
    use unikvm_core::clipboard::Content;
    use unikvm_core::device::DeviceId;
    use unikvm_platform::clipboard::{Clipboard, WriteOutcome};
    use unikvm_protocol::clipboard::{self, READY, Update};

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

    pub fn start(config: &ClipboardConfig, endpoint: Option<&PeerEndpoint>) -> Result<Worker> {
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
                        result = run(connections, &clipboard, local, peer.device_id()) => result,
                    }
                })
                })();
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
    ) -> Result<()> {
        loop {
            let connection = connections
                .wait_for(Option::is_some)
                .await?
                .clone()
                .context("clipboard connection missing")?;
            if let Err(error) = session(&connection, os, local, peer).await {
                debug!(%error, "clipboard session ended");
            }
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
        let sequence = os.sequence()?;
        info!("clipboard sync connected");
        let (updates, outgoing) = watch::channel(None);
        tokio::try_join!(
            send_updates(send, outgoing),
            reconcile(
                os,
                &mut recv,
                updates,
                Revisions::new(local, peer),
                sequence
            )
        )?;
        Ok(())
    }

    async fn send_updates(
        mut send: quinn::SendStream,
        mut updates: watch::Receiver<Option<Arc<Update>>>,
    ) -> Result<()> {
        while updates.changed().await.is_ok() {
            let update = updates.borrow_and_update().clone();
            if let Some(update) = update {
                timeout(UPDATE_TIMEOUT, clipboard::write(&mut send, &update))
                    .await
                    .context("clipboard update write timed out")??;
            }
        }
        Ok(())
    }

    async fn reconcile(
        os: &Clipboard,
        recv: &mut quinn::RecvStream,
        outgoing: watch::Sender<Option<Arc<Update>>>,
        mut revisions: Revisions,
        mut sequence: u64,
    ) -> Result<()> {
        let mut pending: Option<Update> = None;
        let mut poll = interval(POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut was_available = os.available();
        loop {
            let read = clipboard::read(recv);
            tokio::pin!(read);
            loop {
                let update = tokio::select! {
                    result = &mut read => Some(result?),
                    _ = poll.tick() => None,
                };
                let received = update.is_some();
                if !os.available() {
                    if was_available {
                        outgoing.send_replace(None);
                    }
                    was_available = false;
                    pending = None;
                } else {
                    if !was_available {
                        sequence = os.sequence()?;
                        was_available = true;
                    }
                    if let Some(update) = update
                        && revisions.is_new_remote(&update)
                        && pending
                            .as_ref()
                            .is_none_or(|old| update.revision() > old.revision())
                    {
                        pending = Some(update);
                    }
                    // Observe a local change before applying any peer update.
                    // Busy native clipboards are retried on the next tick.
                    if let Ok(snapshot) = os.read(sequence) {
                        if let Some(snapshot) = snapshot {
                            sequence = snapshot.sequence;
                            if !os.available() {
                                outgoing.send_replace(None);
                                pending = None;
                                was_available = false;
                            } else if let Some(content) = snapshot.content.and_then(valid_local) {
                                outgoing
                                    .send_replace(Some(Arc::new(revisions.local_change(content)?)));
                            } else {
                                outgoing.send_replace(None);
                                pending = None;
                            }
                        }
                        if let Some(update) = pending.as_ref() {
                            if !revisions.is_new_remote(update) {
                                pending = None;
                            } else if os.available() {
                                match os.write(sequence, update.content()) {
                                    Ok(WriteOutcome::Applied(applied)) => {
                                        sequence = applied;
                                        revisions.accept_remote(update);
                                        pending = None;
                                    }
                                    Ok(WriteOutcome::Unsupported) => pending = None,
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
