//! Desktop helper for the Explorer receiver. OLE runs on this process's UI thread.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::time::{sleep, timeout};
use unikvm_core::geometry::Edge;
use unikvm_platform::file_drop::run_received;
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::file_drop::{Command, Reply, exchange};

const WAIT: Duration = Duration::from_secs(370);
const POLL: Duration = Duration::from_millis(150);

#[derive(Default)]
struct Delivery {
    requested: bool,
    path: Option<PathBuf>,
}

pub(super) fn receive(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let edge = match args.next().as_deref() {
        None => Edge::Left,
        Some(edge) if edge == "--left" => Edge::Left,
        Some(edge) if edge == "--right" => Edge::Right,
        _ => bail!("usage: unikvm drag-receive [--left | --right]"),
    };
    anyhow::ensure!(
        args.next().is_none(),
        "usage: unikvm drag-receive [--left | --right]"
    );

    let runtime = Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?,
    );
    let Reply::Offer(offer) = runtime.block_on(request(Command::Latest))? else {
        bail!("there is no pending single-file offer from a paired computer");
    };
    let id = offer.id;
    let delivery = Arc::new(Mutex::new(Delivery::default()));
    let result = run_received(edge, &offer.file.name, offer.file.size, {
        let runtime = runtime.clone();
        let delivery = delivery.clone();
        move || {
            let mut delivery = delivery.lock().unwrap();
            if let Some(path) = &delivery.path {
                return Ok(path.clone());
            }
            runtime
                .block_on(receive_path(id, &mut delivery))
                .map_err(io::Error::other)
        }
    });

    let delivery = delivery.lock().unwrap();
    let copied = result.as_ref().is_ok_and(|copied| *copied) && delivery.path.is_some();
    let recorded = if delivery.requested {
        runtime.block_on(request(Command::Finish { id, copied }))? == Reply::Recorded
    } else {
        false
    };
    result.context("run the Explorer file drag")?;
    if copied {
        anyhow::ensure!(recorded, "the agent did not record the drop result");
        println!("Explorer accepted the file copy.");
    } else {
        println!("Drag ended without an accepted file copy.");
    }
    Ok(())
}

async fn receive_path(id: u64, delivery: &mut Delivery) -> Result<PathBuf> {
    if !delivery.requested {
        anyhow::ensure!(
            request(Command::Begin(id)).await? == Reply::Pending,
            "the file offer is no longer available"
        );
        delivery.requested = true;
    }
    let path = timeout(WAIT, async {
        loop {
            match request(Command::Poll(id)).await? {
                Reply::Ready(path) => return Ok::<_, anyhow::Error>(PathBuf::from(path)),
                Reply::Pending => sleep(POLL).await,
                _ => bail!("the file transfer ended before delivery"),
            }
        }
    })
    .await
    .context("waiting for the drop transfer timed out")??;
    anyhow::ensure!(path.is_absolute(), "invalid staged file path");
    delivery.path = Some(path.clone());
    Ok(path)
}

async fn request(command: Command) -> Result<Reply> {
    let endpoint = Endpoint::for_current_user()?;
    timeout(STATUS_TIMEOUT, async {
        let mut stream = endpoint.connect().await.context("connect to local agent")?;
        exchange(&mut stream, command)
            .await
            .context("exchange native drop state")
    })
    .await
    .context("local native drop request timed out")?
}
