//! Desktop helper for the Explorer receiver. OLE runs on this process's UI thread.

use std::ffi::OsString;
#[cfg(target_os = "windows")]
use std::io;
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
#[cfg(target_os = "windows")]
use tokio::time::sleep;
use tokio::time::timeout;
use unikvm_core::geometry::Edge;
#[cfg(target_os = "windows")]
use unikvm_platform::file_drop::run_received;
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::file_drop::{Command, Reply, exchange};

#[cfg(target_os = "windows")]
const WAIT: Duration = Duration::from_secs(370);
#[cfg(target_os = "windows")]
const POLL: Duration = Duration::from_millis(150);

#[derive(Default)]
#[cfg(target_os = "windows")]
struct Delivery {
    requested: bool,
    path: Option<PathBuf>,
}

#[cfg(target_os = "windows")]
pub(super) fn receive(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let edge = match args.next().as_deref() {
        None => Edge::Left,
        Some(edge) if edge == "--left" => Edge::Left,
        Some(edge) if edge == "--right" => Edge::Right,
        _ => bail!("usage: unikvm drag-receive [--left | --right]"),
    };
    let granted = match args.next().as_deref() {
        None => None,
        Some(flag) if flag == "--granted" => Some(
            args.next()
                .and_then(|id| id.to_str()?.parse::<u64>().ok())
                .filter(|id| *id != 0)
                .context("invalid native drop ID")?,
        ),
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
    let Reply::Offer(offer) =
        runtime.block_on(request(granted.map_or(Command::Latest, Command::ForDrag)))?
    else {
        bail!("there is no pending single-file offer from a paired computer");
    };
    let id = offer.id;
    let delivery = Arc::new(Mutex::new(Delivery::default()));
    let window_ready = || {
        if let Some(id) = granted
            && runtime
                .block_on(request(Command::Armed(id)))
                .map_err(io::Error::other)?
                != Reply::Recorded
        {
            return Err(io::Error::other("the native drop is no longer active"));
        }
        Ok(())
    };
    let result = run_received(
        edge,
        &offer.file.name,
        offer.file.size,
        {
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
        },
        window_ready,
    );

    let delivery = delivery.lock().unwrap();
    let copied = result.as_ref().is_ok_and(|copied| *copied) && delivery.path.is_some();
    let recorded = if delivery.requested || granted.is_some() {
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

#[cfg(target_os = "windows")]
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

#[cfg(target_os = "macos")]
pub(super) fn source(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let edge = match args.next().as_deref().and_then(|arg| arg.to_str()) {
        Some("--left") => Edge::Left,
        Some("--right") => Edge::Right,
        _ => bail!("invalid Finder drag edge"),
    };
    anyhow::ensure!(args.next().is_none(), "invalid Finder drag arguments");
    let (events, files) = std::sync::mpsc::sync_channel::<(PathBuf, u64)>(1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let worker = std::thread::Builder::new()
        .name("unikvm Finder drag handoff".into())
        .spawn(move || {
            while let Ok((file, observed_ms)) = files.recv() {
                if let Some(path) = file.to_str() {
                    let _ = runtime.block_on(request(Command::Start {
                        path: path.to_owned(),
                        observed_ms,
                    }));
                }
            }
        })?;
    let result = unikvm_platform::file_drag_source::observe(edge, move |file| {
        use std::time::{SystemTime, UNIX_EPOCH};
        if let Ok(age) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let _ = events.try_send((file, age.as_millis() as u64));
        }
    });
    let _ = worker.join();
    result.context("observe Finder file drags at the display edge")
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
