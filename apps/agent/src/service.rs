use std::io;
use std::process;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::time::timeout;
use tracing::{debug, info, warn};
use unikvm_config::Config;
use unikvm_network::PeerEvent;
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::{ClipboardStatus, Status, respond_with_clipboard};

pub async fn run(config: &Config) -> Result<()> {
    let started = Instant::now();
    let pid = process::id();
    let shutdown = tokio::spawn(shutdown_signal());
    let mut listener = Endpoint::for_current_user()?
        .listen()
        .context("open local agent endpoint")?;
    let network = config.network.as_ref().map(crate::peer::bind).transpose()?;
    let control = config
        .input
        .as_ref()
        .map(|input| crate::input::Control::new(input, network.as_ref()))
        .transpose()?;
    let (clipboard_status, clipboard_view) =
        tokio::sync::watch::channel(ClipboardStatus::default());
    let _clipboard = config
        .clipboard
        .as_ref()
        .map(|config| crate::clipboard::start(config, network.as_ref(), clipboard_status))
        .transpose()?;
    let peer_service = network.map(|endpoint| {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(endpoint.run_until(
            async {
                let _ = stopped.await;
            },
            |event| match event {
                PeerEvent::Connected(peer) => info!(%peer, "authenticated peer connected"),
                PeerEvent::Disconnected(peer) => info!(%peer, "peer disconnected"),
            },
        ));
        (stop, worker)
    });
    info!(pid, "local agent ready");

    let local = async {
        let local = serve(&mut listener, shutdown, pid, started, clipboard_view);
        let control = async {
            if let Some(control) = control
                && let Err(error) = control.run().await
            {
                warn!(%error, "input control stopped");
            }
            std::future::pending::<()>().await;
        };
        tokio::select! {
            result = local => result,
            () = control => unreachable!(),
        }
    }
    .await;
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    drop(_clipboard);
    if let Some((stop, worker)) = peer_service {
        let _ = stop.send(());
        worker.await.context("join peer transport task")?;
    }
    local?;
    info!("local agent stopped");
    Ok(())
}

async fn serve(
    listener: &mut unikvm_platform::local::Listener,
    mut shutdown: tokio::task::JoinHandle<io::Result<()>>,
    pid: u32,
    started: Instant,
    clipboard: tokio::sync::watch::Receiver<ClipboardStatus>,
) -> Result<()> {
    loop {
        let stream = tokio::select! {
            signal = &mut shutdown => {
                signal??;
                break;
            }
            result = listener.accept() => result.context("accept local connection")?,
        };
        let exchange = respond_with_clipboard(
            stream,
            || Status {
                pid,
                uptime_secs: started.elapsed().as_secs(),
            },
            || *clipboard.borrow(),
        );
        tokio::select! {
            signal = &mut shutdown => {
                signal??;
                break;
            }
            result = timeout(STATUS_TIMEOUT, exchange) => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => debug!(%error, "local status exchange failed"),
                    Err(_) => debug!("local status exchange timed out"),
                }
            }
        }
        listener.finish().context("close local status exchange")?;
    }
    Ok(())
}

#[cfg(unix)]
async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(target_os = "windows")]
async fn shutdown_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
}
