use std::io;
use std::process;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::time::timeout;
use tracing::{debug, info};
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::{Status, respond};

pub async fn run() -> Result<()> {
    let started = Instant::now();
    let pid = process::id();
    let shutdown = tokio::spawn(shutdown_signal());
    tokio::pin!(shutdown);
    let mut listener = Endpoint::for_current_user()?
        .listen()
        .context("open local agent endpoint")?;
    info!(pid, "local agent ready");

    loop {
        let stream = tokio::select! {
            signal = &mut shutdown => {
                signal??;
                break;
            }
            result = listener.accept() => result.context("accept local connection")?,
        };
        let exchange = respond(stream, || Status {
            pid,
            uptime_secs: started.elapsed().as_secs(),
        });
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
    info!("local agent stopped");
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
