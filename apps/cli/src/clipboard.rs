#[cfg(any(target_os = "macos", target_os = "windows"))]
mod desktop {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::{thread, time::Duration};

    use anyhow::{Context, Result};
    use tokio::runtime::Builder;
    use tokio::sync::{mpsc, oneshot, watch};
    use tokio::time::{MissedTickBehavior, interval, timeout};
    use unikvm_platform::clipboard_monitor::{self, Snapshot};
    use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
    use unikvm_protocol::local::{ApprovalDecision, approval, decide, query_tray, tray_command};
    use unikvm_protocol::tray::Command;

    pub fn open(agent_owned: bool) -> Result<()> {
        let endpoint = Endpoint::for_current_user().context("locate local agent endpoint")?;
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let (update, view) = watch::channel(None);
        let (stop, mut stopped) = oneshot::channel();
        let (send, mut commands) = mpsc::channel(8);
        let close = Arc::new(AtomicBool::new(false));
        let worker_close = close.clone();
        let worker = thread::Builder::new()
            .name("unikvm tray status".into())
            .spawn(move || {
                runtime.block_on(async {
                    let mut poll = interval(Duration::from_millis(150));
                    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    let mut unavailable = 0;
                let mut seen_agent = false;
                    loop {
                        tokio::select! {
                            biased;
                            _ = &mut stopped => break,
                            Some(command) = commands.recv() => {
                                let accepted = timeout(STATUS_TIMEOUT, async {
                                    let mut stream = endpoint.connect().await?;
                                    tray_command(&mut stream, command).await
                                }).await;
                            if command == Command::Exit && (matches!(accepted, Ok(Ok(true))) || update.borrow().is_none()) {
                                    worker_close.store(true, Ordering::Relaxed);
                                    break;
                                }
                            }
                            _ = poll.tick() => {
                                let status = timeout(STATUS_TIMEOUT, async {
                                    let mut stream = endpoint.connect().await?;
                                    query_tray(&mut stream).await
                                }).await.ok().and_then(Result::ok);
                                if status.is_some() {
                                    unavailable = 0;
                                seen_agent = true;
                                } else {
                                    unavailable += 1;
                                }
                            if status.is_some() || unavailable >= 3 {
                                update.send_replace(status);
                            }
                            if (seen_agent && unavailable >= 3) || (agent_owned && unavailable >= 20) {
                                    worker_close.store(true, Ordering::Relaxed);
                                    break;
                                }
                            }
                        }
                    }
                });
            })
            .context("start tray status worker")?;
        let result = clipboard_monitor::show(
            move || Snapshot {
                view: view.borrow().clone(),
                close: close.load(Ordering::Relaxed),
            },
            move |command| {
                let _ = send.try_send(command);
            },
        );
        let _ = stop.send(());
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("tray status worker stopped unexpectedly"))?;
        result.context("show native tray")
    }

    pub fn prompt(id: u64) -> Result<()> {
        let endpoint = Endpoint::for_current_user()?;
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let offer = runtime.block_on(async {
            timeout(STATUS_TIMEOUT, async {
                let mut stream = endpoint.connect().await?;
                approval(&mut stream, id).await
            })
            .await?
        })?;
        let Some(offer) = offer else { return Ok(()) };
        let accept = clipboard_monitor::approve(&offer.label, offer.bytes)?;
        runtime.block_on(async {
            timeout(STATUS_TIMEOUT, async {
                let mut stream = endpoint.connect().await?;
                decide(&mut stream, ApprovalDecision { id, accept }).await
            })
            .await?
        })?;
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use desktop::{open, prompt};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn open(_: bool) -> anyhow::Result<()> {
    anyhow::bail!("the native tray is available on Windows and macOS")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn prompt(_: u64) -> anyhow::Result<()> {
    open(false)
}
