use std::env;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use tokio::time::timeout;
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::{Status, query};

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("unikvm: {error:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let command = args.next();
    if args.next().is_some() {
        bail!("usage: unikvm status");
    }
    match command.as_deref().and_then(|arg| arg.to_str()) {
        Some("status") => {}
        Some("-h" | "--help") => {
            println!("Usage: unikvm status");
            return Ok(());
        }
        _ => bail!("usage: unikvm status"),
    }

    let status = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(read_status())?;
    println!(
        "Agent ready (PID {}, up {}s)",
        status.pid, status.uptime_secs
    );
    Ok(())
}

async fn read_status() -> Result<Status> {
    let endpoint = Endpoint::for_current_user().context("locate local agent endpoint")?;
    timeout(STATUS_TIMEOUT, async {
        let mut stream = endpoint.connect().await.context("connect to local agent")?;
        query(&mut stream).await.context("read local agent status")
    })
    .await
    .context("local agent status timed out")?
}
