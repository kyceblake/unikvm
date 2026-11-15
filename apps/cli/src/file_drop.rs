//! Desktop helpers for native file drags. OLE and AppKit run on their platform UI threads.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::time::{sleep, timeout};
use unikvm_core::geometry::Edge;
use unikvm_platform::file_drop::run_received;
#[cfg(target_os = "windows")]
use unikvm_platform::file_drop::run_received_return;
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
    let mode = args.next();
    let returned = mode.as_deref().is_some_and(|flag| flag == "--returned");
    let granted = match mode.as_deref() {
        None => None,
        Some(flag) if flag == "--granted" || flag == "--returned" => Some(
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
    #[cfg(target_os = "macos")]
    anyhow::ensure!(
        granted.is_some() && !returned,
        "Finder file drops start automatically from a paired drag"
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
        #[cfg(target_os = "windows")]
        if returned {
            runtime
                .block_on(async {
                    timeout(Duration::from_secs(5), async {
                        loop {
                            match request(Command::AwaitLocal(id)).await? {
                                Reply::Recorded => return Ok::<_, anyhow::Error>(()),
                                Reply::Pending => sleep(Duration::from_millis(15)).await,
                                _ => bail!("the returned drag is no longer active"),
                            }
                        }
                    })
                    .await
                    .context("returned drag activation timed out")?
                })
                .map_err(io::Error::other)?;
        }
        Ok(())
    };
    #[cfg(target_os = "windows")]
    let run = if returned {
        run_received_return
    } else {
        run_received
    };
    #[cfg(target_os = "macos")]
    let run = run_received;
    let result = run(
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
                    .block_on(receive_path(id, returned, &mut delivery))
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
    result.context("run the native file drag")?;
    if copied {
        anyhow::ensure!(recorded, "the agent did not record the drop result");
        let destination = if cfg!(target_os = "macos") {
            "Finder"
        } else {
            "Explorer"
        };
        println!("{destination} accepted the file copy.");
    } else {
        println!("Drag ended without an accepted file copy.");
    }
    Ok(())
}

async fn receive_path(id: u64, returned: bool, delivery: &mut Delivery) -> Result<PathBuf> {
    if !delivery.requested {
        if returned {
            anyhow::ensure!(
                request(Command::Released(id)).await? == Reply::Recorded,
                "the returned drag was not released after activation"
            );
        }
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

pub(super) fn source(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    #[cfg(target_os = "macos")]
    use unikvm_platform::file_drag_source::observe;
    #[cfg(target_os = "windows")]
    use unikvm_platform::file_drop::observe;
    let edge = match args.next().as_deref().and_then(|arg| arg.to_str()) {
        Some("--left") => Edge::Left,
        Some("--right") => Edge::Right,
        _ => bail!("invalid file drag edge"),
    };
    anyhow::ensure!(args.next().is_none(), "invalid file drag arguments");
    let (events, files) = std::sync::mpsc::sync_channel::<(PathBuf, u64)>(1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let worker = std::thread::Builder::new()
        .name("unikvm file drag handoff".into())
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
    let result = observe(edge, move |file| {
        use std::time::{SystemTime, UNIX_EPOCH};
        if let Ok(age) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let _ = events.try_send((file, age.as_millis() as u64));
        }
    });
    let _ = worker.join();
    result.context("observe file drags at the display edge")
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
