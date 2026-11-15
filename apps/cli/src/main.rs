mod clipboard;
mod identity;

use std::env;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use tokio::time::timeout;
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::{Status, query};

use crate::identity::{USAGE, identity_command, verify_command};

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
    match command.as_deref().and_then(|arg| arg.to_str()) {
        Some("status") if args.next().is_none() => {}
        Some("clipboard") if args.next().is_none() => return clipboard::open(),
        Some("clipboard-approval") => {
            let id = args
                .next()
                .and_then(|value| value.to_str()?.parse::<u64>().ok())
                .context("expected a clipboard approval identifier")?;
            anyhow::ensure!(args.next().is_none(), USAGE);
            return clipboard::prompt(id);
        }
        Some("identity") => return identity_command(args),
        Some("peer") => return verify_command(args),
        Some("-h" | "--help") if args.next().is_none() => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => bail!(USAGE),
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
