use std::{
    cell::{Cell, RefCell},
    io, process,
    time::Instant,
};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tracing::{debug, info, warn};
use unikvm_config::Config;
use unikvm_network::{PeerEndpoint, PeerEvent, identity::Fingerprint};
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::{
    Approval, ApprovalDecision, ClipboardStatus, Status, file_drop, respond_with_desktop,
};
use unikvm_protocol::tray::{Command, Connection};

use crate::desktop::Desktop;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Requested {
    Online,
    Offline,
    Exit,
}

struct Local {
    desktop: Desktop,
    requested: watch::Sender<Requested>,
    status: watch::Sender<ClipboardStatus>,
    offer: watch::Sender<Option<Approval>>,
    decisions: RefCell<Option<mpsc::Sender<ApprovalDecision>>>,
}

pub async fn run(config: &Config) -> Result<()> {
    let started = Instant::now();
    let pid = process::id();
    let mut listener = Endpoint::for_current_user()?
        .listen()
        .context("open local agent endpoint")?;
    let initial = config.network.as_ref().map(crate::peer::bind).transpose()?;
    let (desktop, pins) = Desktop::new(config)?;
    let (requested, requests) = watch::channel(if initial.is_some() {
        Requested::Online
    } else {
        Requested::Offline
    });
    let local = Local {
        desktop,
        requested,
        status: watch::channel(ClipboardStatus::default()).0,
        offer: watch::channel(None).0,
        decisions: RefCell::new(None),
    };

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let helper = crate::desktop::Helper::start()
        .inspect_err(|_| debug!("could not start the tray helper"))
        .ok();
    info!(pid, "local agent ready");
    let (result, ()) = tokio::join!(
        async {
            let result = serve(&mut listener, pid, started, &local).await;
            local.requested.send_replace(Requested::Exit);
            result
        },
        supervise(config, initial, requests, &local, &pins),
    );
    drop(listener);
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if let Some(helper) = helper {
        helper.stop().await;
    }
    result?;
    info!("local agent stopped");
    Ok(())
}

async fn supervise(
    config: &Config,
    mut initial: Option<PeerEndpoint>,
    mut requests: watch::Receiver<Requested>,
    local: &Local,
    pins: &[Fingerprint],
) {
    let Some(network) = &config.network else {
        return;
    };
    loop {
        let requested = *requests.borrow_and_update();
        match requested {
            Requested::Exit => return,
            Requested::Offline => {
                let _ = requests.changed().await;
                continue;
            }
            Requested::Online => {}
        }
        local
            .desktop
            .view
            .send_modify(|view| view.connection = Connection::Connecting);
        let result = match initial
            .take()
            .map_or_else(|| crate::peer::bind(network), Ok)
        {
            Ok(endpoint) => generation(config, endpoint, &mut requests, local, pins).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            warn!(%error, "peer service stopped");
        }
        local.decisions.replace(None);
        local.offer.send_replace(None);
        local.status.send_replace(ClipboardStatus::default());
        local.desktop.finish_transfers();
        local.desktop.view.send_modify(|view| {
            view.connection = Connection::Disconnected;
            for peer in &mut view.peers {
                peer.connected = false;
            }
        });
        local.requested.send_if_modified(|request| {
            if *request != Requested::Online {
                return false;
            }
            *request = Requested::Offline;
            true
        });
    }
}

async fn generation(
    config: &Config,
    endpoint: PeerEndpoint,
    requests: &mut watch::Receiver<Requested>,
    local: &Local,
    pins: &[Fingerprint],
) -> Result<()> {
    let control = config
        .input
        .as_ref()
        .map(|input| crate::input::Control::new(input, Some(&endpoint)))
        .transpose()?;
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let paste = config
        .clipboard
        .as_ref()
        .filter(|clipboard| clipboard.files)
        .map(|_| unikvm_platform::clipboard::PasteRequests::default());
    let (decide, decisions) = mpsc::channel(4);
    let clipboard = config
        .clipboard
        .as_ref()
        .map(|config| {
            crate::clipboard::start(
                config,
                Some(&endpoint),
                local.status.clone(),
                local.offer.clone(),
                decisions,
                #[cfg(any(target_os = "windows", target_os = "macos"))]
                (paste.clone(), local.desktop.clone(), pins),
            )
        })
        .transpose()?;
    local.decisions.replace(Some(decide));
    let (stop, stopped) = oneshot::channel();
    let mut network = tokio::spawn({
        let desktop = local.desktop.clone();
        let pins = pins.to_vec();
        endpoint.run_until(
            async {
                let _ = stopped.await;
            },
            move |event| {
                let (pin, connected) = match event {
                    PeerEvent::Connected(peer) => {
                        info!(%peer, "authenticated peer connected");
                        (peer, true)
                    }
                    PeerEvent::Disconnected(peer) => {
                        info!(%peer, "peer disconnected");
                        (peer, false)
                    }
                };
                if let Some(index) = pins.iter().position(|peer| *peer == pin) {
                    desktop
                        .view
                        .send_modify(|view| view.peers[index].connected = connected);
                }
            },
        )
    });
    local
        .desktop
        .view
        .send_modify(|view| view.connection = Connection::Online);
    let ended = {
        let input = async {
            if let Some(control) = control {
                let result = control
                    .run(
                        #[cfg(any(target_os = "windows", target_os = "macos"))]
                        paste.clone(),
                        #[cfg(any(target_os = "windows", target_os = "macos"))]
                        local.desktop.clone(),
                    )
                    .await;
                if let Err(error) = result {
                    warn!(%error, "input control stopped");
                }
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            let _monitor = paste.and_then(|requests| {
                unikvm_platform::input::Capture::for_paste(requests)
                    .inspect_err(|error| warn!(%error, "clipboard paste shortcut unavailable"))
                    .ok()
            });
            std::future::pending::<()>().await;
        };
        tokio::select! {
            _ = requests.wait_for(|request| *request != Requested::Online) => None,
            result = &mut network => Some(result),
            () = input => unreachable!(),
        }
    };
    local.desktop.finish_transfers();
    let _ = stop.send(());
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if let Some(worker) = clipboard {
        worker.stop().await;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = clipboard;
    if let Some(result) = ended {
        result.context("join peer transport task")?;
    } else {
        network.await.context("join peer transport task")?;
    }
    Ok(())
}

impl Local {
    fn command(&self, command: Command, exit: &Cell<bool>) -> bool {
        match command {
            Command::Connect
                if self.desktop.view.borrow().connection == Connection::Disconnected =>
            {
                self.desktop
                    .view
                    .send_modify(|view| view.connection = Connection::Connecting);
                self.requested.send_replace(Requested::Online);
                true
            }
            Command::Disconnect
                if matches!(
                    self.desktop.view.borrow().connection,
                    Connection::Online | Connection::Connecting
                ) =>
            {
                self.desktop
                    .view
                    .send_modify(|view| view.connection = Connection::Disconnecting);
                self.requested.send_replace(Requested::Offline);
                true
            }
            Command::Exit => {
                exit.set(true);
                true
            }
            Command::Cancel(id) => self.desktop.cancel(id),
            _ => false,
        }
    }
}

async fn serve(
    listener: &mut unikvm_platform::local::Listener,
    pid: u32,
    started: Instant,
    local: &Local,
) -> Result<()> {
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        let stream = tokio::select! {
            signal = &mut shutdown => { signal?; break; }
            result = listener.accept() => result.context("accept local connection")?,
        };
        let exit = Cell::new(false);
        let exchange = respond_with_desktop(
            stream,
            || Status {
                pid,
                uptime_secs: started.elapsed().as_secs(),
            },
            || *local.status.borrow(),
            |id| {
                local
                    .offer
                    .borrow()
                    .as_ref()
                    .filter(|offer| offer.id == id)
                    .cloned()
            },
            |decision| {
                local
                    .offer
                    .borrow()
                    .as_ref()
                    .is_some_and(|offer| offer.id == decision.id)
                    && local
                        .decisions
                        .borrow()
                        .as_ref()
                        .is_some_and(|sender| sender.try_send(decision).is_ok())
            },
            || {
                let mut view = local.desktop.view.borrow().clone();
                let clipboard = local.status.borrow();
                view.clipboard = [clipboard.outgoing, clipboard.incoming];
                view
            },
            (
                |command| local.command(command, &exit),
                |drop_command| {
                    #[cfg(target_os = "macos")]
                    if let file_drop::Command::Start {
                        ref path,
                        observed_ms,
                    } = drop_command
                        && *local.requested.borrow() == Requested::Online
                    {
                        use std::time::{SystemTime, UNIX_EPOCH};
                        let path = std::path::PathBuf::from(path);
                        let fresh = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .is_ok_and(|age| {
                                age.as_millis()
                                    .checked_sub(u128::from(observed_ms))
                                    .is_some_and(|elapsed| elapsed <= 500)
                            });
                        if fresh
                            && path.is_absolute()
                            && local.desktop.drag_starts.send(path).is_ok()
                        {
                            return file_drop::Reply::Recorded;
                        }
                        return file_drop::Reply::Unavailable;
                    }
                    #[cfg(any(target_os = "windows", target_os = "macos"))]
                    if *local.requested.borrow() == Requested::Online {
                        return local.desktop.file_drop.command(drop_command);
                    }
                    let _ = drop_command;
                    file_drop::Reply::Unavailable
                },
            ),
        );
        tokio::select! {
            signal = &mut shutdown => { signal?; break; }
            result = timeout(STATUS_TIMEOUT, exchange) => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => debug!(%error, "local status exchange failed"),
                    Err(_) => debug!("local status exchange timed out"),
                }
            }
        }
        listener.finish().context("close local status exchange")?;
        if exit.get() {
            break;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::UdpSocket, time::Duration};
    use unikvm_network::identity::{Identity, TrustedPeer};

    #[tokio::test]
    async fn desktop_disconnect_closes_the_local_peer_connection() {
        let local_id = Identity::generate().unwrap();
        let remote_id = Identity::generate().unwrap();
        let sockets = [(); 2].map(|()| UdpSocket::bind("127.0.0.1:0").unwrap());
        let [local_addr, remote_addr] = sockets
            .each_ref()
            .map(|socket| socket.local_addr().unwrap());
        drop(sockets);
        let approve = |id: &Identity| {
            TrustedPeer::approve(id.certificate().to_vec(), id.fingerprint()).unwrap()
        };
        let local_endpoint = PeerEndpoint::bind(
            local_addr,
            false,
            &local_id,
            vec![(approve(&remote_id), Some(remote_addr))],
        )
        .unwrap();
        let remote_endpoint = PeerEndpoint::bind(
            remote_addr,
            false,
            &remote_id,
            vec![(approve(&local_id), Some(local_addr))],
        )
        .unwrap();
        let mut remote_connection = remote_endpoint.connection(local_id.fingerprint()).unwrap();
        let (stop, stopped) = oneshot::channel();
        let remote = tokio::spawn(remote_endpoint.run_until(
            async {
                let _ = stopped.await;
            },
            |_| {},
        ));
        let config = Config::parse(&format!(
            "schema_version = 1\n[network]\nlisten = '{local_addr}'\nidentity = 'identity.bin'\n\
            [[network.peers]]\naddress = '{remote_addr}'\ncertificate = 'peer.der'\nfingerprint = '{}'\n",
            remote_id.fingerprint(),
        )).unwrap();
        let (desktop, pins) = Desktop::new(&config).unwrap();
        let (requested, requests) = watch::channel(Requested::Online);
        let local = Local {
            desktop,
            requested,
            status: watch::channel(ClipboardStatus::default()).0,
            offer: watch::channel(None).0,
            decisions: RefCell::new(None),
        };
        let disconnect = async {
            local
                .desktop
                .view
                .subscribe()
                .wait_for(|view| view.peers[0].connected)
                .await
                .unwrap();
            remote_connection.wait_for(Option::is_some).await.unwrap();
            assert!(local.command(Command::Disconnect, &Cell::new(false)));
            remote_connection.wait_for(Option::is_none).await.unwrap();
            local
                .desktop
                .view
                .subscribe()
                .wait_for(|view| view.connection == Connection::Disconnected)
                .await
                .unwrap();
            assert!(!local.desktop.view.borrow().peers[0].connected);
            assert!(!remote.is_finished());
            local.requested.send_replace(Requested::Exit);
        };
        timeout(Duration::from_secs(8), async {
            tokio::join!(
                supervise(&config, Some(local_endpoint), requests, &local, &pins),
                disconnect
            );
        })
        .await
        .unwrap();
        let _ = stop.send(());
        remote.await.unwrap();
    }
}
