use std::future::{Future, pending};
use std::num::NonZeroU64;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval, timeout};
use tracing::{debug, info, warn};
use unikvm_clipboard::Revisions;
use unikvm_config::ClipboardConfig;
use unikvm_core::clipboard::Content;
use unikvm_core::device::DeviceId;
use unikvm_network::PeerEndpoint;
use unikvm_platform::clipboard::{Clipboard, WriteOutcome, open_source, staging_directory};
use unikvm_protocol::clipboard::{self, Control, DATA, Layout, Offer, READY};
use unikvm_protocol::local::{
    Approval, ApprovalDecision, ClipboardPhase as Phase, ClipboardStatus, Progress,
};
use unikvm_transfer::{self as transfer, Prepared, Received};

use super::approval::{Prompt, make_approval};
use super::staging::Staging;

const POLL: Duration = Duration::from_millis(150);
const HANDSHAKE: Duration = Duration::from_secs(5);
const APPROVAL: Duration = Duration::from_secs(60);
const IN_FLIGHT: Duration = Duration::from_secs(300);
const APPLY: Duration = Duration::from_secs(30);

type Job<T> = Pin<Box<dyn Future<Output = Result<T>>>>;

pub struct Worker(Option<oneshot::Sender<()>>);

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            let _ = stop.send(());
        }
    }
}

pub fn start(
    config: &ClipboardConfig,
    endpoint: Option<&PeerEndpoint>,
    status: watch::Sender<ClipboardStatus>,
    approval: watch::Sender<Option<Approval>>,
    decisions: mpsc::Receiver<ApprovalDecision>,
) -> Result<Worker> {
    let peer = config.peer_fingerprint.parse()?;
    let endpoint = endpoint.context("clipboard sharing requires a network endpoint")?;
    let connections = endpoint
        .connection(peer)
        .context("clipboard peer is not trusted")?;
    let (stop, stopped) = oneshot::channel();
    let mut worker = Desktop {
        local: endpoint.fingerprint().device_id(),
        peer: peer.device_id(),
        files: config.files,
        status,
        approval,
        decisions,
        staging: Staging::default(),
    };
    // Native clipboard reads may wait on the owning application. Keep them off
    // the input and network runtime.
    std::thread::Builder::new()
        .name("unikvm clipboard".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()?;
                let os = Clipboard::new()?;
                if worker.files {
                    match staging_directory() {
                        Ok(directory) => worker.staging.directory = Some(directory),
                        Err(_) => {
                            worker.files = false;
                            warn!("file sharing disabled because the clipboard staging directory is unavailable");
                        }
                    }
                }
                runtime.block_on(async {
                    tokio::select! {
                        _ = stopped => Ok(()),
                        result = worker.run(connections, &os) => result,
                    }
                })
            })();
            if result.is_err() {
                warn!("clipboard sync stopped");
            }
        })?;
    Ok(Worker(Some(stop)))
}

struct Desktop {
    local: DeviceId,
    peer: DeviceId,
    files: bool,
    status: watch::Sender<ClipboardStatus>,
    approval: watch::Sender<Option<Approval>>,
    decisions: mpsc::Receiver<ApprovalDecision>,
    staging: Staging,
}

impl Desktop {
    async fn run(
        mut self,
        mut connections: watch::Receiver<Option<quinn::Connection>>,
        os: &Clipboard,
    ) -> Result<()> {
        let mut previous = None;
        let mut poll = interval(POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            let connection = tokio::select! {
                ready = connections.wait_for(|value| value.as_ref().is_some_and(|conn| Some(conn.stable_id()) != previous)) => {
                    ready?.clone().context("clipboard connection missing")?
                }
                _ = poll.tick() => {
                    self.staging.reconcile(os);
                    continue;
                }
            };
            previous = Some(connection.stable_id());
            if self.session(connection, os).await.is_err() {
                debug!("clipboard session ended");
            }
            self.approval.send_replace(None);
            self.status.send_modify(|status| {
                status.connected = false;
                for phase in [&mut status.outgoing, &mut status.incoming] {
                    *phase = if matches!(*phase, Phase::Copying | Phase::Waiting) {
                        Phase::Failed
                    } else {
                        Phase::Idle
                    };
                }
            });
        }
    }

    async fn session(&mut self, connection: quinn::Connection, os: &Clipboard) -> Result<()> {
        let (send, recv, files) = timeout(HANDSHAKE, handshake(&connection, self.files)).await??;
        let (control, outgoing) = mpsc::channel(16);
        let (incoming, events) = mpsc::channel(16);
        self.staging.reconcile(os);
        let sequence = os.sequence()?;
        let mut link = Link {
            revisions: Revisions::new(self.local, self.peer),
            desktop: self,
            connection,
            control,
            os,
            files,
            sequence,
            out: None,
            incoming: None,
            newest: None,
            send_job: None,
            receive_job: None,
            available: os.available(),
        };
        link.desktop.status.send_replace(ClipboardStatus {
            connected: true,
            ..ClipboardStatus::default()
        });
        info!("clipboard sync connected");
        tokio::try_join!(
            write_controls(send, outgoing),
            read_controls(recv, incoming),
            link.run(events)
        )?;
        Ok(())
    }
}

async fn handshake(
    connection: &quinn::Connection,
    files: bool,
) -> Result<(quinn::SendStream, quinn::RecvStream, bool)> {
    let mut send = connection.open_uni().await?;
    send.set_priority(1)?;
    send.write_all(READY).await?;
    send.write_u8(u8::from(files)).await?;
    let mut recv = connection.accept_uni().await?;
    let mut header = [0; READY.len()];
    recv.read_exact(&mut header).await?;
    ensure!(header == READY, "clipboard protocol versions differ");
    let remote_files = recv.read_u8().await?;
    ensure!(remote_files <= 1, "invalid clipboard capability");
    Ok((send, recv, files && remote_files == 1))
}

async fn write_controls(
    mut send: quinn::SendStream,
    mut controls: mpsc::Receiver<Control>,
) -> Result<()> {
    while let Some(control) = controls.recv().await {
        timeout(HANDSHAKE, clipboard::write(&mut send, &control)).await??;
    }
    Ok(())
}

async fn read_controls(mut recv: quinn::RecvStream, controls: mpsc::Sender<Control>) -> Result<()> {
    loop {
        // Wait indefinitely for the first byte of the next record, but bound
        // the time allowed to finish that record once it begins.
        let tag = recv.read_u8().await?;
        let mut frame = std::io::Cursor::new([tag]).chain(&mut recv);
        let control = timeout(HANDSHAKE, clipboard::read(&mut frame)).await??;
        controls
            .send(control)
            .await
            .context("clipboard control receiver closed")?;
    }
}

struct Outgoing {
    id: NonZeroU64,
    prepared: Option<Prepared>,
    deadline: Instant,
}

struct Incoming {
    offer: Offer,
    deadline: Instant,
    accepted: bool,
    prompt: Option<Prompt>,
    ready: Option<Received>,
}

struct Link<'a> {
    desktop: &'a mut Desktop,
    os: &'a Clipboard,
    files: bool,
    connection: quinn::Connection,
    revisions: Revisions,
    sequence: u64,
    available: bool,
    control: mpsc::Sender<Control>,
    out: Option<Outgoing>,
    incoming: Option<Incoming>,
    newest: Option<NonZeroU64>,
    send_job: Option<Job<()>>,
    receive_job: Option<Job<Option<Received>>>,
}

impl Link<'_> {
    async fn run(&mut self, mut events: mpsc::Receiver<Control>) -> Result<()> {
        let mut poll = interval(POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = self.connection.closed() => return Ok(()),
                message = events.recv() => {
                    let Some(message) = message else { return Ok(()); };
                    self.read_clipboard()?;
                    self.control_received(message)?;
                }
                decision = self.desktop.decisions.recv() => {
                    if let Some(decision) = decision {
                        self.read_clipboard()?;
                        self.decide(decision)?;
                    }
                }
                stream = self.connection.accept_uni() => self.data_stream(stream?),
                result = poll_job(&mut self.send_job) => {
                    self.send_job = None;
                    if result.is_err() {
                        self.withdraw(Phase::Failed)?;
                    } else if let Some(out) = self.out.as_mut() {
                        out.deadline = Instant::now() + APPLY + HANDSHAKE;
                    }
                }
                result = poll_job(&mut self.receive_job) => {
                    self.receive_job = None;
                    self.read_clipboard()?;
                    if let Some(incoming) = self.incoming.as_mut() {
                        match result {
                            Ok(Some(data)) => {
                                incoming.ready = Some(data);
                                incoming.deadline = Instant::now() + APPLY;
                                self.apply()?;
                            }
                            Ok(None) => {}
                            Err(_) => self.finish_incoming(Phase::Failed, true)?,
                        }
                    }
                }
                _ = poll.tick() => {
                    self.read_clipboard()?;
                    self.desktop.staging.expire();
                    let now = Instant::now();
                    if self.incoming.as_ref().is_some_and(|item| item.deadline <= now) {
                        self.finish_incoming(Phase::Failed, true)?;
                    }
                    if self.out.as_ref().is_some_and(|item| item.deadline <= now) {
                        self.withdraw(Phase::Failed)?;
                    }
                    self.apply()?;
                }
            }
        }
    }

    fn tell(&self, control: Control) -> Result<()> {
        self.control
            .try_send(control)
            .map_err(|_| anyhow::anyhow!("clipboard control queue unavailable"))
    }

    fn control_received(&mut self, control: Control) -> Result<()> {
        match control {
            Control::Offer(offer) => self.offer(offer),
            Control::Decision { id, accept } => {
                let Some(out) = self.out.as_mut().filter(|item| item.id == id) else {
                    return Ok(());
                };
                let Some(prepared) = out.prepared.take() else {
                    return Ok(());
                };
                if !accept {
                    self.out = None;
                    self.desktop
                        .status
                        .send_modify(|s| s.outgoing = Phase::Declined);
                    return Ok(());
                }
                out.deadline = Instant::now() + IN_FLIGHT;
                let connection = self.connection.clone();
                let status = self.desktop.status.clone();
                status.send_modify(|s| s.outgoing = Phase::Copying);
                self.send_job = Some(Box::pin(async move {
                    let mut send = timeout(HANDSHAKE, connection.open_uni()).await??;
                    send.set_priority(-1)?;
                    timeout(HANDSHAKE, async {
                        send.write_all(DATA).await?;
                        send.write_u64(id.get()).await
                    })
                    .await??;
                    transfer::send(&mut send, prepared, |bytes| {
                        status.send_modify(|s| s.sent.bytes = bytes);
                    })
                    .await?;
                    send.finish()?;
                    Ok(())
                }));
                Ok(())
            }
            Control::Receipt { id, applied } => {
                if self
                    .out
                    .as_ref()
                    .is_some_and(|item| item.id == id && item.prepared.is_none())
                {
                    self.out = None;
                    self.send_job = None;
                    self.desktop.status.send_modify(|s| {
                        s.outgoing = if applied { Phase::Ready } else { Phase::Failed }
                    });
                }
                Ok(())
            }
            Control::Withdraw(id) => {
                if self
                    .incoming
                    .as_ref()
                    .is_some_and(|item| item.offer.revision.counter == id)
                {
                    self.finish_incoming(Phase::Failed, false)?;
                }
                Ok(())
            }
        }
    }

    fn offer(&mut self, offer: Offer) -> Result<()> {
        ensure!(
            offer.revision.origin == self.desktop.peer,
            "clipboard offer origin mismatch"
        );
        let id = offer.revision.counter;
        let total = offer.layout.total()?;
        let requires_approval = offer.layout.requires_approval()?;
        if !self.available
            || (matches!(offer.layout, Layout::Files(_)) && !self.files)
            || !self.revisions.is_new_remote(offer.revision)
            || self.newest.is_some_and(|previous| previous >= id)
        {
            return self.tell(Control::Decision { id, accept: false });
        }
        self.newest = Some(id);
        self.finish_incoming(Phase::Failed, true)?;
        let mut incoming = Incoming {
            offer,
            deadline: Instant::now() + APPROVAL,
            accepted: false,
            ready: None,
            prompt: None,
        };
        if requires_approval {
            let approval = make_approval(&incoming.offer.layout, total)?;
            self.desktop.approval.send_replace(Some(approval.clone()));
            match Prompt::launch(approval.id) {
                Ok(prompt) => incoming.prompt = Some(prompt),
                Err(_) => {
                    self.desktop.approval.send_replace(None);
                    self.desktop
                        .status
                        .send_modify(|s| s.incoming = Phase::Failed);
                    warn!(
                        "could not open clipboard approval prompt; build the UniKVM CLI beside the agent"
                    );
                    return self.tell(Control::Decision { id, accept: false });
                }
            }
        }
        self.desktop.status.send_modify(|s| {
            s.received = Progress { bytes: 0, total };
            s.incoming = if requires_approval {
                Phase::Waiting
            } else {
                Phase::Copying
            };
        });
        self.incoming = Some(incoming);
        if !requires_approval {
            self.accept()?;
        }
        Ok(())
    }

    fn decide(&mut self, decision: ApprovalDecision) -> Result<()> {
        if !self
            .desktop
            .approval
            .borrow()
            .as_ref()
            .is_some_and(|offer| offer.id == decision.id)
            || !self
                .incoming
                .as_ref()
                .is_some_and(|incoming| !incoming.accepted && incoming.deadline > Instant::now())
        {
            return Ok(());
        }
        if decision.accept {
            self.accept()
        } else {
            self.finish_incoming(Phase::Declined, true)
        }
    }

    fn accept(&mut self) -> Result<()> {
        let Some(incoming) = self.incoming.as_mut() else {
            return Ok(());
        };
        incoming.accepted = true;
        incoming.deadline = Instant::now() + IN_FLIGHT;
        incoming.prompt = None;
        self.desktop.approval.send_replace(None);
        self.desktop
            .status
            .send_modify(|s| s.incoming = Phase::Copying);
        let id = incoming.offer.revision.counter;
        self.tell(Control::Decision { id, accept: true })
    }

    fn data_stream(&mut self, mut receive: quinn::RecvStream) {
        let Some(incoming) = self
            .incoming
            .as_ref()
            .filter(|item| item.accepted && item.ready.is_none())
        else {
            let _ = receive.stop(0u32.into());
            return;
        };
        if self.receive_job.is_some() {
            let _ = receive.stop(0u32.into());
            return;
        }
        let id = incoming.offer.revision.counter;
        let layout = incoming.offer.layout.clone();
        let status = self.desktop.status.clone();
        let directory = self.desktop.staging.directory.clone();
        self.receive_job = Some(Box::pin(async move {
            let mut header = [0; DATA.len()];
            timeout(HANDSHAKE, receive.read_exact(&mut header)).await??;
            ensure!(header == DATA, "invalid clipboard payload header");
            if timeout(HANDSHAKE, receive.read_u64()).await?? != id.get() {
                // A withdrawn offer can leave a data stream in flight.
                let _ = receive.stop(0u32.into());
                return Ok(None);
            }
            Ok(Some(
                transfer::receive(&mut receive, &layout, directory.as_deref(), |bytes| {
                    status.send_modify(|s| s.received.bytes = bytes);
                })
                .await?,
            ))
        }));
    }

    fn read_clipboard(&mut self) -> Result<()> {
        if !self.os.available() {
            if self.available {
                self.available = false;
                self.withdraw(Phase::Failed)?;
                self.finish_incoming(Phase::Failed, true)?;
            }
            return Ok(());
        }
        if !self.available {
            self.available = true;
            self.desktop.staging.reconcile(self.os);
            self.sequence = self.os.sequence()?;
            return Ok(());
        }
        let Ok(Some(snapshot)) = self.os.read(self.sequence) else {
            return Ok(());
        };
        self.sequence = snapshot.sequence;
        if snapshot.imported {
            self.desktop.staging.sequence = snapshot.sequence;
            return Ok(());
        }
        self.desktop.staging.publish(None, snapshot.sequence);
        self.finish_incoming(Phase::Failed, true)?;
        self.withdraw(Phase::Skipped)?;
        let revision = self.revisions.local_change_after(self.newest)?;
        self.desktop.status.send_modify(|s| {
            s.outgoing = Phase::Skipped;
            s.sent = Progress::default();
            if s.incoming == Phase::Ready {
                s.incoming = Phase::Idle;
            }
        });
        let Some(content) = snapshot.content else {
            return Ok(());
        };
        if !self.os.available() || (matches!(content, Content::Files(_)) && !self.files) {
            return Ok(());
        }
        let prepared = match Prepared::new(content, open_source) {
            Ok(prepared) => prepared,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::FileTooLarge {
                    self.desktop
                        .status
                        .send_modify(|s| s.outgoing = Phase::TooLarge);
                }
                return Ok(());
            }
        };
        let total = prepared.layout.total()?;
        let requires_approval = prepared.layout.requires_approval()?;
        self.tell(Control::Offer(Offer {
            revision,
            layout: prepared.layout.clone(),
        }))?;
        self.out = Some(Outgoing {
            id: revision.counter,
            prepared: Some(prepared),
            deadline: Instant::now() + APPROVAL + HANDSHAKE,
        });
        self.desktop.status.send_modify(|s| {
            s.outgoing = if requires_approval {
                Phase::Waiting
            } else {
                Phase::Copying
            };
            s.sent = Progress { bytes: 0, total };
        });
        Ok(())
    }

    fn apply(&mut self) -> Result<()> {
        let Some(incoming) = self.incoming.as_mut() else {
            return Ok(());
        };
        let Some(data) = incoming.ready.as_ref() else {
            return Ok(());
        };
        if !self.available || !self.revisions.is_new_remote(incoming.offer.revision) {
            return self.finish_incoming(Phase::Failed, true);
        }
        match self.os.write(self.sequence, &data.content) {
            Ok(WriteOutcome::Applied(sequence)) => {
                self.sequence = sequence;
                self.revisions.accept_remote(incoming.offer.revision);
                self.desktop.staging.publish(
                    incoming
                        .ready
                        .take()
                        .and_then(|item| item.staging)
                        .map(|staging| staging.keep()),
                    sequence,
                );
                self.finish_incoming(Phase::Ready, true)
            }
            Ok(WriteOutcome::Unsupported) => self.finish_incoming(Phase::Failed, true),
            // A temporarily-busy clipboard can be retried until APPLY.
            Ok(WriteOutcome::Changed) | Err(_) => Ok(()),
        }
    }

    fn finish_incoming(&mut self, phase: Phase, notify: bool) -> Result<()> {
        if let Some(item) = self.incoming.take() {
            self.receive_job = None;
            self.desktop.approval.send_replace(None);
            self.desktop.status.send_modify(|s| s.incoming = phase);
            if notify {
                let id = item.offer.revision.counter;
                let reply = if item.accepted {
                    Control::Receipt {
                        id,
                        applied: phase == Phase::Ready,
                    }
                } else {
                    Control::Decision { id, accept: false }
                };
                self.tell(reply)?;
            }
        }
        Ok(())
    }

    fn withdraw(&mut self, phase: Phase) -> Result<()> {
        if let Some(item) = self.out.take() {
            self.send_job = None;
            self.desktop.status.send_modify(|s| s.outgoing = phase);
            self.tell(Control::Withdraw(item.id))?;
        }
        Ok(())
    }
}

async fn poll_job<T>(job: &mut Option<Job<T>>) -> Result<T> {
    match job {
        Some(job) => job.await,
        None => pending().await,
    }
}
