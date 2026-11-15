#[cfg(any(target_os = "macos", target_os = "windows"))]
mod desktop {
    use std::thread;
    use std::time::Duration;

    use anyhow::{Context, Result};
    use tokio::runtime::Builder;
    use tokio::sync::{oneshot, watch};
    use tokio::time::{MissedTickBehavior, interval, timeout};
    use unikvm_platform::clipboard_monitor::{self, Row, View};
    use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
    use unikvm_protocol::local::{ClipboardPhase, ClipboardStatus, query_clipboard};

    enum Direction {
        Outgoing,
        Incoming,
    }

    pub fn open() -> Result<()> {
        let endpoint = Endpoint::for_current_user().context("locate local agent endpoint")?;
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let (update, view) = watch::channel(View {
            connection: "Checking the local agent…",
            ..display(None)
        });
        let (stop, mut stopped) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("unikvm clipboard status".into())
            .spawn(move || {
                runtime.block_on(async {
                    let mut poll = interval(Duration::from_millis(150));
                    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            _ = &mut stopped => break,
                            _ = poll.tick() => {}
                        }
                        let status = tokio::select! {
                            _ = &mut stopped => break,
                            status = read_status(&endpoint) => status,
                        };
                        update.send_replace(display(status));
                    }
                });
            })
            .context("start clipboard status worker")?;
        let result = clipboard_monitor::show(move || *view.borrow());
        let _ = stop.send(());
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("clipboard status worker stopped unexpectedly"))?;
        result.context("show native clipboard monitor")
    }

    async fn read_status(endpoint: &Endpoint) -> Option<ClipboardStatus> {
        timeout(STATUS_TIMEOUT, async {
            let mut stream = endpoint.connect().await?;
            query_clipboard(&mut stream).await
        })
        .await
        .ok()?
        .ok()
    }

    fn display(status: Option<ClipboardStatus>) -> View {
        let connection = match status {
            None => "Clipboard status unavailable. Check that the agent is running.",
            Some(state) if state.connected => "Paired clipboard connected",
            Some(_) => "Clipboard sharing is disconnected or disabled",
        };
        let state = status.unwrap_or_default();
        View {
            connection,
            rows: [
                row(Direction::Outgoing, state.outgoing),
                row(Direction::Incoming, state.incoming),
            ],
        }
    }

    fn row(direction: Direction, phase: ClipboardPhase) -> Row {
        use ClipboardPhase::{Copying, Failed, Idle, Ready, Skipped};
        use Direction::{Incoming, Outgoing};

        let text = match (direction, phase) {
            (_, Idle) => "Waiting for a copy",
            (Outgoing, Copying) => "Copying to the other computer…",
            (Incoming, Copying) => "Receiving clipboard…",
            (Outgoing, Ready) => "Ready to paste on the other computer",
            (Incoming, Ready) => "Ready to paste here",
            (Outgoing, Failed) => "Copy couldn’t be confirmed",
            (Incoming, Failed) => "Couldn’t put the copy on this clipboard",
            (_, Skipped) => "This clipboard entry was skipped",
        };
        Row {
            text,
            copying: phase == Copying,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use desktop::open;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn open() -> anyhow::Result<()> {
    anyhow::bail!("the native clipboard monitor is available on Windows and macOS");
}
