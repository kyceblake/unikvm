use std::cell::{Cell, RefCell};
use std::future::{Future, pending};
use std::num::NonZeroU64;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::{JoinSet, LocalSet};
use tokio::time::{Instant, MissedTickBehavior, interval, timeout};
use tracing::{debug, info, warn};
use unikvm_clipboard::Revisions;
use unikvm_config::ClipboardConfig;
use unikvm_core::clipboard::Content;
use unikvm_core::device::DeviceId;
use unikvm_network::{PeerEndpoint, identity::Fingerprint};
use unikvm_platform::clipboard::{
    Clipboard, PasteRequests, Snapshot, WriteOutcome, open_source, staging_directory,
};
use unikvm_protocol::clipboard::{self, Control, DATA, Layout, Offer, READY};
use unikvm_protocol::local::{
    Approval, ApprovalDecision, ClipboardPhase as Phase, ClipboardStatus, Progress, file_drop,
};
use unikvm_protocol::tray::{Phase as UiPhase, Transfer};
use unikvm_transfer::{self as transfer, Prepared, Received};

use super::approval::{Prompt, make_approval};
use super::staging::Staging;

const POLL: Duration = Duration::from_millis(150);
const HANDSHAKE: Duration = Duration::from_secs(5);
const APPROVAL: Duration = Duration::from_secs(60);
const FILE_OFFER: Duration = Duration::from_secs(600);
const IN_FLIGHT: Duration = Duration::from_secs(300);
const APPLY: Duration = Duration::from_secs(30);

type Job<T> = Pin<Box<dyn Future<Output = Result<T>>>>;

pub struct Worker {
    signal: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub async fn stop(mut self) {
        if let Some(stop) = self.signal.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(stop) = self.signal.take() {
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
    (paste, ui, pins): (
        Option<PasteRequests>,
        crate::desktop::Desktop,
        &[Fingerprint],
    ),
) -> Result<Worker> {
    let endpoint = endpoint.context("clipboard sharing requires a network endpoint")?;
    let peers = config
        .peers()
        .map(|peer| {
            let fingerprint: Fingerprint = peer.parse()?;
            let connections = endpoint
                .connection(fingerprint)
                .context("clipboard peer is not trusted")?;
            let name = pins
                .iter()
                .position(|pin| *pin == fingerprint)
                .and_then(|index| {
                    ui.view
                        .borrow()
                        .peers
                        .get(index)
                        .map(|peer| peer.name.clone())
                })
                .context("clipboard peer has no desktop status")?;
            Ok((fingerprint.device_id(), name, connections))
        })
        .collect::<Result<Vec<_>>>()?;
    let (stop, stopped) = oneshot::channel();
    let local = endpoint.fingerprint().device_id();
    // Native clipboard reads may wait on the owning application. Keep them off
    // the input and network runtime.
    let thread = std::thread::Builder::new()
        .name("unikvm clipboard".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()?;
                let os = Rc::new(Clipboard::new()?);
                let mut paste = paste;
                let mut staging = Staging::default();
                if paste.is_some() {
                    match staging_directory() {
                        Ok(directory) => staging.directory = Some(directory),
                        Err(_) => {
                            paste = None;
                            warn!("file sharing disabled because the clipboard staging directory is unavailable");
                        }
                    }
                }
                let worker = Rc::new(Desktop {
                    local,
                    status,
                    approval,
                    paste,
                    ui,
                    staging: RefCell::new(staging),
                    snapshot: RefCell::new(None),
                    local_sequence: Cell::new(None),
                    incoming: Cell::new(None),
                    next_token: Cell::new(0),
                    send_slot: std::sync::Arc::new(Semaphore::new(1)),
                    peer_status: RefCell::new(vec![PeerStatus::default(); peers.len()]),
                });
                LocalSet::new().block_on(&runtime, async {
                    tokio::select! {
                        _ = stopped => Ok(()),
                        result = worker.run(peers, decisions, os) => result,
                    }
                })
            })();
            if result.is_err() {
                warn!("clipboard sync stopped");
            }
        })?;
    Ok(Worker {
        signal: Some(stop),
        thread: Some(thread),
    })
}

struct Desktop {
    local: DeviceId,
    status: watch::Sender<ClipboardStatus>,
    approval: watch::Sender<Option<Approval>>,
    paste: Option<PasteRequests>,
    ui: crate::desktop::Desktop,
    staging: RefCell<Staging>,
    snapshot: RefCell<Option<Rc<Snapshot>>>,
    local_sequence: Cell<Option<u64>>,
    incoming: Cell<Option<u64>>,
    next_token: Cell<u64>,
    send_slot: std::sync::Arc<Semaphore>,
    peer_status: RefCell<Vec<PeerStatus>>,
}

type PeerConnection = watch::Receiver<Option<quinn::Connection>>;

#[derive(Clone, Copy, Default)]
struct PeerStatus {
    connected: bool,
    sequence: Option<u64>,
    phase: Phase,
    progress: Progress,
}

impl Desktop {
    async fn run(
        self: Rc<Self>,
        peers: Vec<(DeviceId, String, PeerConnection)>,
        mut decisions: mpsc::Receiver<ApprovalDecision>,
        os: Rc<Clipboard>,
    ) -> Result<()> {
        self.staging.borrow_mut().reconcile(&os);
        let (routes, _) = broadcast::channel(8);
        let mut links = JoinSet::new();
        for (index, (peer, name, connections)) in peers.into_iter().enumerate() {
            links.spawn_local(self.clone().run_peer(
                index,
                (peer, name),
                connections,
                routes.subscribe(),
                os.clone(),
            ));
        }
        let mut poll = interval(POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                Some(result) = links.join_next() => {
                    result??;
                    return Ok(());
                }
                Some(decision) = decisions.recv() => { let _ = routes.send(decision); }
                _ = poll.tick() => {
                    let mut staging = self.staging.borrow_mut();
                    if self.status.borrow().connected {
                        staging.expire();
                    } else {
                        staging.reconcile(&os);
                    }
                }
            }
        }
    }

    async fn run_peer(
        self: Rc<Self>,
        index: usize,
        peer: (DeviceId, String),
        mut connections: PeerConnection,
        mut decisions: broadcast::Receiver<ApprovalDecision>,
        os: Rc<Clipboard>,
    ) -> Result<()> {
        // Quinn's connection ID is tied to the lifetime of its allocation.
        let mut previous: Option<quinn::Connection> = None;
        loop {
            let connection = connections
                .wait_for(|value| {
                    value.as_ref().is_some_and(|conn| {
                        previous
                            .as_ref()
                            .is_none_or(|old| old.stable_id() != conn.stable_id())
                    })
                })
                .await?
                .clone()
                .context("clipboard connection missing")?;
            previous = Some(connection.clone());
            if self
                .session(index, &peer, connection, &os, &mut decisions)
                .await
                .is_err()
            {
                debug!("clipboard session ended");
            }
            self.update_peer(index, |state| {
                state.connected = false;
                if matches!(
                    state.phase,
                    Phase::Offered | Phase::Waiting | Phase::Copying
                ) {
                    state.phase = Phase::Failed;
                }
            });
        }
    }

    async fn session(
        self: &Rc<Self>,
        index: usize,
        peer: &(DeviceId, String),
        connection: quinn::Connection,
        os: &Clipboard,
        decisions: &mut broadcast::Receiver<ApprovalDecision>,
    ) -> Result<()> {
        let (send, recv, files) =
            timeout(HANDSHAKE, handshake(&connection, self.paste.is_some())).await??;
        let (control, outgoing) = mpsc::channel(16);
        let (incoming, events) = mpsc::channel(16);
        let sequence = os.sequence()?;
        let mut link = Link {
            revisions: Revisions::new(self.local, peer.0),
            desktop: self.clone(),
            index,
            peer: peer.0,
            peer_name: &peer.1,
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
        self.update_peer(index, |state| {
            *state = PeerStatus {
                connected: true,
                ..PeerStatus::default()
            }
        });
        info!("clipboard sync connected");
        tokio::try_join!(
            write_controls(send, outgoing),
            read_controls(recv, incoming),
            link.run(events, decisions, self.ui.cancels.subscribe())
        )?;
        Ok(())
    }

    fn update_peer(&self, index: usize, update: impl FnOnce(&mut PeerStatus)) {
        let mut states = self.peer_status.borrow_mut();
        update(&mut states[index]);
        let connected = states.iter().any(|state| state.connected);
        let best = states
            .iter()
            .filter(|state| state.sequence == self.local_sequence.get())
            .max_by_key(|state| (outgoing_rank(state.phase), state.progress.bytes))
            .copied()
            .unwrap_or_default();
        self.status.send_modify(|status| {
            status.connected = connected;
            status.outgoing = best.phase;
            status.sent = best.progress;
        });
    }

    fn snapshot(&self, os: &Clipboard, previous: u64) -> Option<Rc<Snapshot>> {
        let sequence = os.sequence().ok()?;
        if sequence == previous {
            return None;
        }
        let mut cache = self.snapshot.borrow_mut();
        if let Some(snapshot) = cache
            .as_ref()
            .filter(|snapshot| snapshot.sequence == sequence)
        {
            return Some(snapshot.clone());
        }
        *cache = Some(Rc::new(os.read(previous).ok()??));
        cache.clone()
    }

    fn local_copy(&self, sequence: u64) {
        if self.local_sequence.replace(Some(sequence)) != Some(sequence) {
            self.staging.borrow_mut().publish(None, sequence);
            if let Some(token) = self.incoming.get() {
                self.end_incoming(token, Phase::Idle);
            }
            self.status
                .send_modify(|status| status.incoming = Phase::Idle);
        }
    }

    fn paste_requested(&self, token: u64) -> bool {
        self.paste
            .as_ref()
            .is_some_and(|paste| paste.requested(token))
    }

    fn select_incoming(&self, phase: Phase, total: u64) -> Option<u64> {
        if let Some(token) = self.incoming.get() {
            if matches!(
                self.status.borrow().incoming,
                Phase::Waiting | Phase::Copying
            ) || self
                .paste
                .as_ref()
                .is_some_and(|paste| !paste.clear_unrequested(token))
            {
                return None;
            }
            self.end_incoming(token, Phase::Idle);
        }
        let token = self.next_token.get().wrapping_add(1);
        self.next_token.set(token);
        self.incoming.set(Some(token));
        self.status.send_modify(|status| {
            status.received = Progress { bytes: 0, total };
            status.incoming = phase;
        });
        Some(token)
    }

    fn end_incoming(&self, token: u64, phase: Phase) {
        self.ui.file_drop.clear(token);
        if self.incoming.get() == Some(token) {
            self.incoming.set(None);
            if let Some(paste) = &self.paste {
                paste.clear(token);
            }
            self.approval.send_replace(None);
            self.status.send_modify(|status| status.incoming = phase);
        }
    }
}

fn outgoing_rank(phase: Phase) -> u8 {
    match phase {
        Phase::Copying => 8,
        Phase::Waiting => 7,
        Phase::Ready => 6,
        Phase::Offered => 5,
        Phase::TooLarge => 4,
        Phase::Declined => 3,
        Phase::Failed => 2,
        Phase::Skipped => 1,
        Phase::Idle => 0,
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
    awaiting_request: bool,
    transfer: Option<u64>,
    slot: Option<OwnedSemaphorePermit>,
}

impl Outgoing {
    fn request(&mut self, id: NonZeroU64, now: Instant) -> bool {
        if self.id != id || !self.awaiting_request || self.deadline <= now {
            return false;
        }
        self.awaiting_request = false;
        self.deadline = now + APPROVAL + HANDSHAKE;
        true
    }

    fn decision(&mut self, id: NonZeroU64, accept: bool, now: Instant) -> Option<Prepared> {
        if self.id != id || self.deadline <= now || (accept && self.awaiting_request) {
            return None;
        }
        self.prepared.take()
    }
}

struct Incoming {
    offer: Offer,
    token: u64,
    sequence: u64,
    deadline: Instant,
    accepted: bool,
    prompt: Option<Prompt>,
    ready: Option<Received>,
    transfer: Option<u64>,
    native: bool,
}

struct Link<'a> {
    desktop: Rc<Desktop>,
    index: usize,
    peer: DeviceId,
    peer_name: &'a str,
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
    send_job: Option<Job<OwnedSemaphorePermit>>,
    receive_job: Option<Job<Option<Received>>>,
}

impl Link<'_> {
    async fn run(
        &mut self,
        mut events: mpsc::Receiver<Control>,
        decisions: &mut broadcast::Receiver<ApprovalDecision>,
        mut cancels: broadcast::Receiver<u64>,
    ) -> Result<()> {
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
                decision = decisions.recv() => {
                    if let Ok(decision) = decision {
                        self.read_clipboard()?;
                        self.decide(decision)?;
                    }
                }
                cancel = cancels.recv() => {
                    if let Ok(id) = cancel {
                        if self.out.as_ref().is_some_and(|item| item.transfer == Some(id)) {
                            self.finish_ui(0, Some(id), UiPhase::Cancelled);
                            self.withdraw(Phase::Failed)?;
                        } else if self.incoming.as_ref().is_some_and(|item| item.transfer == Some(id)) {
                            self.finish_ui(1, Some(id), UiPhase::Cancelled);
                            self.finish_incoming(Phase::Failed, true)?;
                        }
                    }
                }
                stream = self.connection.accept_uni() => self.data_stream(stream?),
                result = poll_job(&mut self.send_job) => {
                    self.send_job = None;
                    match result {
                        Err(_) => self.withdraw(Phase::Failed)?,
                        Ok(slot) => if let Some(out) = self.out.as_mut() {
                            out.slot = Some(slot);
                            out.deadline = Instant::now() + APPLY + HANDSHAKE;
                        }
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
                    let now = Instant::now();
                    if self.incoming.as_ref().is_some_and(|item| item.deadline <= now) {
                        self.finish_incoming(Phase::Failed, true)?;
                    }
                    self.poll_file_request()?;
                    if self.out.as_ref().is_some_and(|item| item.deadline <= now) {
                        self.withdraw(Phase::Failed)?;
                    }
                    self.apply()?;
                }
            }
            if self.incoming.as_ref().is_some_and(|item| !self.owns(item)) {
                self.finish_incoming(Phase::Failed, true)?;
            }
        }
    }

    fn owns(&self, item: &Incoming) -> bool {
        self.desktop.incoming.get() == Some(item.token)
    }

    fn poll_file_request(&mut self) -> Result<()> {
        let Some(incoming) = self.incoming.as_mut() else {
            return Ok(());
        };
        let native = &self.desktop.ui.file_drop;
        if !incoming.offer.layout.is_file() || incoming.prompt.is_some() || incoming.accepted {
            return Ok(());
        }
        if native.claim(incoming.token) {
            if let Some(paste) = &self.desktop.paste
                && paste.clear_unrequested(incoming.token)
            {
                incoming.native = true;
            } else {
                native.clear(incoming.token);
            }
        }
        if incoming.native || self.desktop.paste_requested(incoming.token) {
            if self.os.sequence()? == incoming.sequence {
                self.show_prompt()?;
            } else {
                self.finish_incoming(Phase::Failed, true)?;
            }
        }
        Ok(())
    }

    fn outgoing_phase(&self, phase: Phase) {
        self.desktop
            .update_peer(self.index, |status| status.phase = phase);
    }

    fn finish_ui(&self, direction: usize, id: Option<u64>, phase: UiPhase) {
        if let Some(id) = id {
            self.desktop.ui.update_transfer(direction, id, |item| {
                item.phase = phase;
                if matches!(phase, UiPhase::Ready | UiPhase::Dropped) {
                    item.bytes = item.total;
                }
            });
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
            Control::Request(id) => {
                if self
                    .out
                    .as_mut()
                    .is_some_and(|item| item.request(id, Instant::now()))
                {
                    self.outgoing_phase(Phase::Waiting);
                }
                Ok(())
            }
            Control::Decision { id, accept } => {
                let Some(out) = self.out.as_mut() else {
                    return Ok(());
                };
                let now = Instant::now();
                let Some(prepared) = out.decision(id, accept, now) else {
                    return Ok(());
                };
                if !accept {
                    self.out = None;
                    self.outgoing_phase(Phase::Declined);
                    return Ok(());
                }
                out.deadline = now + IN_FLIGHT;
                let presentation = transfer_view(&prepared.layout, self.peer_name)?;
                out.transfer = presentation.as_ref().map(|transfer| transfer.id);
                let connection = self.connection.clone();
                let desktop = self.desktop.clone();
                let index = self.index;
                self.outgoing_phase(Phase::Copying);
                self.send_job = Some(Box::pin(async move {
                    let slot = desktop.send_slot.clone().acquire_owned().await?;
                    let transfer_id = presentation.as_ref().map(|transfer| transfer.id);
                    if let Some(transfer) = presentation {
                        desktop.ui.transfer(0, transfer);
                    }
                    let mut send = timeout(HANDSHAKE, connection.open_uni()).await??;
                    send.set_priority(-1)?;
                    timeout(HANDSHAKE, async {
                        send.write_all(DATA).await?;
                        send.write_u64(id.get()).await
                    })
                    .await??;
                    transfer::send(&mut send, prepared, |bytes| {
                        desktop.update_peer(index, |s| s.progress.bytes = bytes);
                        if let Some(id) = transfer_id {
                            desktop
                                .ui
                                .update_transfer(0, id, |transfer| transfer.bytes = bytes);
                        }
                    })
                    .await?;
                    send.finish()?;
                    Ok(slot)
                }));
                Ok(())
            }
            Control::Receipt { id, applied } => {
                if self
                    .out
                    .as_ref()
                    .is_some_and(|item| item.id == id && item.prepared.is_none())
                {
                    let transfer = self.out.take().and_then(|item| item.transfer);
                    self.finish_ui(
                        0,
                        transfer,
                        if applied {
                            UiPhase::Ready
                        } else {
                            UiPhase::Failed
                        },
                    );
                    self.send_job = None;
                    self.outgoing_phase(if applied { Phase::Ready } else { Phase::Failed });
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
            offer.revision.origin == self.peer,
            "clipboard offer origin mismatch"
        );
        let id = offer.revision.counter;
        let total = offer.layout.total()?;
        let requires_approval = offer.layout.requires_approval()?;
        let file = offer.layout.is_file();
        if !self.available
            || (file && !self.files)
            || !self.revisions.is_new_remote(offer.revision)
            || self.newest.is_some_and(|previous| previous >= id)
        {
            return self.tell(Control::Decision { id, accept: false });
        }
        self.newest = Some(id);
        self.finish_incoming(Phase::Failed, true)?;
        let phase = if file {
            Phase::Offered
        } else if requires_approval {
            Phase::Waiting
        } else {
            Phase::Copying
        };
        let Some(token) = self.desktop.select_incoming(phase, total) else {
            return self.tell(Control::Decision { id, accept: false });
        };
        let deadline = Instant::now() + if file { FILE_OFFER } else { APPROVAL };
        if cfg!(windows)
            && let Layout::Files(files) = &offer.layout
            && let [file] = files.as_slice()
            && let Ok(approval) = make_approval(&offer.layout, total)
            && approval.id != 0
        {
            self.desktop.ui.file_drop.offer(
                token,
                file_drop::Offer {
                    id: approval.id,
                    file: file.clone(),
                },
                deadline.into_std(),
            );
        }
        self.incoming = Some(Incoming {
            offer,
            token,
            sequence: self.sequence,
            deadline,
            accepted: false,
            ready: None,
            prompt: None,
            transfer: None,
            native: false,
        });
        if file {
            if let Some(paste) = &self.desktop.paste {
                paste.arm(token, self.sequence, deadline.into_std());
            }
        } else if requires_approval {
            self.show_prompt()?;
        } else {
            self.accept()?;
        }
        Ok(())
    }

    fn show_prompt(&mut self) -> Result<()> {
        let Some(incoming) = self.incoming.as_mut() else {
            return Ok(());
        };
        if !incoming.native {
            self.desktop.ui.file_drop.clear(incoming.token);
        }
        let approval = make_approval(&incoming.offer.layout, incoming.offer.layout.total()?)?;
        let approval_id = approval.id;
        self.desktop.approval.send_replace(Some(approval));
        match Prompt::launch(approval_id) {
            Ok(prompt) => incoming.prompt = Some(prompt),
            Err(_) => {
                warn!(
                    "could not open clipboard approval prompt; build the UniKVM CLI beside the agent"
                );
                return self.finish_incoming(Phase::Failed, true);
            }
        }
        incoming.deadline = Instant::now() + APPROVAL;
        let file_request = incoming
            .offer
            .layout
            .is_file()
            .then_some(incoming.offer.revision.counter);
        self.desktop
            .status
            .send_modify(|s| s.incoming = Phase::Waiting);
        if let Some(id) = file_request {
            self.tell(Control::Request(id))?;
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
            || !self.incoming.as_ref().is_some_and(|incoming| {
                self.owns(incoming)
                    && incoming.prompt.is_some()
                    && !incoming.accepted
                    && incoming.deadline > Instant::now()
            })
        {
            return Ok(());
        }
        if decision.accept {
            if self.incoming.as_ref().map(|incoming| incoming.sequence) != self.os.sequence().ok() {
                return self.finish_incoming(Phase::Failed, true);
            }
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
        if let Some(transfer) = transfer_view(&incoming.offer.layout, self.peer_name)? {
            incoming.transfer = Some(transfer.id);
            self.desktop.ui.transfer(1, transfer);
        }
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
            .filter(|item| self.owns(item) && item.accepted && item.ready.is_none())
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
        let ui = self.desktop.ui.clone();
        let transfer = incoming.transfer;
        let directory = self.desktop.staging.borrow().directory.clone();
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
                    if let Some(id) = transfer {
                        ui.update_transfer(1, id, |transfer| transfer.bytes = bytes);
                    }
                })
                .await?,
            ))
        }));
    }

    fn read_clipboard(&mut self) -> Result<()> {
        if let Some(incoming) = self.incoming.as_ref()
            && let Some(copied) = self.desktop.ui.file_drop.finished(incoming.token)
        {
            // Explorer has already copied the file. Record it before processing a newer copy.
            if copied {
                self.revisions.accept_remote(incoming.offer.revision);
            }
            self.finish_incoming(if copied { Phase::Ready } else { Phase::Failed }, true)?;
        }
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
            self.sequence = self.os.sequence()?;
            return Ok(());
        }
        let Some(snapshot) = self.desktop.snapshot(self.os, self.sequence) else {
            return Ok(());
        };
        self.sequence = snapshot.sequence;
        if snapshot.imported {
            return Ok(());
        }
        self.desktop.local_copy(snapshot.sequence);
        self.finish_incoming(Phase::Failed, true)?;
        self.withdraw(Phase::Skipped)?;
        let revision = self.revisions.local_change_after(self.newest)?;
        self.desktop.update_peer(self.index, |s| {
            s.sequence = Some(snapshot.sequence);
            s.phase = Phase::Skipped;
            s.progress = Progress::default();
        });
        let Some(content) = snapshot.content.clone() else {
            return Ok(());
        };
        if !self.os.available() || (matches!(content, Content::Files(_)) && !self.files) {
            return Ok(());
        }
        let prepared = match Prepared::new(content, open_source) {
            Ok(prepared) => prepared,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::FileTooLarge {
                    self.outgoing_phase(Phase::TooLarge);
                }
                return Ok(());
            }
        };
        let total = prepared.layout.total()?;
        let requires_approval = prepared.layout.requires_approval()?;
        let file = prepared.layout.is_file();
        self.tell(Control::Offer(Offer {
            revision,
            layout: prepared.layout.clone(),
        }))?;
        self.out = Some(Outgoing {
            id: revision.counter,
            prepared: Some(prepared),
            deadline: Instant::now() + if file { FILE_OFFER } else { APPROVAL } + HANDSHAKE,
            awaiting_request: file,
            transfer: None,
            slot: None,
        });
        self.desktop.update_peer(self.index, |s| {
            s.phase = if file {
                Phase::Offered
            } else if requires_approval {
                Phase::Waiting
            } else {
                Phase::Copying
            };
            s.progress = Progress { bytes: 0, total };
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
        if self.desktop.incoming.get() != Some(incoming.token)
            || !self.available
            || !self.revisions.is_new_remote(incoming.offer.revision)
        {
            return self.finish_incoming(Phase::Failed, true);
        }
        if incoming.native {
            let Content::Files(paths) = &data.content else {
                return self.finish_incoming(Phase::Failed, true);
            };
            if let [path] = paths.as_slice()
                && let Some(path) = path
                    .to_str()
                    .filter(|path| path.len() <= file_drop::MAX_PATH_BYTES)
                && self.desktop.ui.file_drop.ready(incoming.token, path)
            {
                return Ok(());
            }
            return self.finish_incoming(Phase::Failed, true);
        }
        match self.os.write(incoming.sequence, &data.content) {
            Ok(WriteOutcome::Applied(sequence)) => {
                self.sequence = sequence;
                self.revisions.accept_remote(incoming.offer.revision);
                self.desktop.staging.borrow_mut().publish(
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
            self.desktop.end_incoming(item.token, phase);
            self.finish_ui(
                1,
                item.transfer,
                if phase == Phase::Ready && item.native {
                    UiPhase::Dropped
                } else if phase == Phase::Ready {
                    UiPhase::Ready
                } else {
                    UiPhase::Failed
                },
            );
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
            self.outgoing_phase(phase);
            self.finish_ui(0, item.transfer, UiPhase::Failed);
            self.tell(Control::Withdraw(item.id))?;
        }
        Ok(())
    }
}

impl Drop for Link<'_> {
    fn drop(&mut self) {
        if let Some(incoming) = &self.incoming {
            self.desktop.end_incoming(incoming.token, Phase::Failed);
            self.finish_ui(1, incoming.transfer, UiPhase::Failed);
        }
        if let Some(out) = &self.out {
            self.finish_ui(0, out.transfer, UiPhase::Failed);
        }
    }
}

fn transfer_view(layout: &Layout, peer: &str) -> Result<Option<Transfer>> {
    let Layout::Files(files) = layout else {
        return Ok(None);
    };
    let total = layout.total()?;
    let presentation = make_approval(layout, total)?;
    Ok(Some(Transfer {
        id: presentation.id.max(1),
        peer: peer.to_owned(),
        label: presentation.label,
        items: files.len() as u8,
        phase: UiPhase::Copying,
        bytes: 0,
        total,
    }))
}

async fn poll_job<T>(job: &mut Option<Job<T>>) -> Result<T> {
    match job {
        Some(job) => job.await,
        None => pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_decision_is_bound_to_the_request_on_each_peer() {
        let now = Instant::now();
        let id = NonZeroU64::MIN;
        let wrong_id = NonZeroU64::new(2).unwrap();
        let pending = || {
            let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
            Outgoing {
                id,
                prepared: Some(Prepared::new(Content::Files(vec![fixture]), open_source).unwrap()),
                deadline: now + FILE_OFFER,
                awaiting_request: true,
                transfer: None,
                slot: None,
            }
        };

        let mut first = pending();
        let mut second = pending();
        assert!(first.decision(id, true, now).is_none());
        assert!(!first.request(wrong_id, now));
        assert!(first.request(id, now));
        assert!(!first.request(id, now + APPROVAL));
        assert!(first.decision(wrong_id, true, now).is_none());
        assert!(first.decision(id, true, now).is_some());
        assert!(first.decision(id, true, now).is_none());
        assert!(second.decision(id, true, now).is_none());
        assert!(second.prepared.is_some());
        assert!(second.decision(id, false, now).is_some());

        let mut expired = pending();
        assert!(!expired.request(id, now + FILE_OFFER));
        let mut unanswered = pending();
        assert!(unanswered.request(id, now));
        assert!(
            unanswered
                .decision(id, true, now + APPROVAL + HANDSHAKE)
                .is_none()
        );
    }
}
