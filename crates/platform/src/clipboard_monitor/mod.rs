//! Native desktop tray and file transfer UI.

use std::{
    io,
    time::{Duration, Instant},
};
use unikvm_protocol::{
    local::ClipboardPhase,
    tray::{Command, Connection, Phase, Transfer, View},
};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

const TITLE: &str = "UniKVM";
const UI_POLL_MS: u64 = 100;
const PEER_SUMMARY_SPACE: usize = 15;

pub struct Snapshot {
    pub view: Option<View>,
    pub close: bool,
}

pub fn show(
    read: impl Fn() -> Snapshot + 'static,
    command: impl Fn(Command) + 'static,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::show(read, command);
    #[cfg(target_os = "windows")]
    return windows::show(read, command);
}

pub fn approve(label: &str, bytes: u64) -> io::Result<bool> {
    let size = size(bytes);
    #[cfg(target_os = "macos")]
    return macos::approve(label, &size);
    #[cfg(target_os = "windows")]
    return windows::approve(label, &size);
}

fn connection(view: Option<&View>) -> &'static str {
    match view.map(|view| view.connection) {
        None => "Agent unavailable",
        Some(Connection::Disabled) => "No paired devices",
        Some(Connection::Disconnected) => "Disconnected",
        Some(Connection::Connecting) => "Connecting…",
        Some(Connection::Online) => "Online",
        Some(Connection::Disconnecting) => "Disconnecting…",
    }
}

fn peer_label(peer: &unikvm_protocol::tray::Peer) -> String {
    let state = if peer.connected {
        "Connected"
    } else {
        "Offline"
    };
    format!("{} — {state}", peer.name)
}

fn clipboard_status(view: &View, direction: usize) -> Option<String> {
    let sending = direction == 0;
    let phase = view.clipboard[direction];
    if matches!(
        phase,
        ClipboardPhase::Offered
            | ClipboardPhase::Waiting
            | ClipboardPhase::TooLarge
            | ClipboardPhase::Declined
    ) {
        return Some(
            match phase {
                ClipboardPhase::Offered if sending => "File offered. Paste on a receiving device",
                ClipboardPhase::Offered if cfg!(target_os = "macos") => {
                    "Press ⌘V to request the file"
                }
                ClipboardPhase::Offered => "Press Ctrl+V to request the file",
                ClipboardPhase::Waiting => "Waiting for approval",
                ClipboardPhase::TooLarge => "Copy exceeds the 50 MB limit",
                _ => "Copy declined",
            }
            .to_owned(),
        );
    }
    view.transfers[direction]
        .as_ref()
        .map(|item| match item.phase {
            Phase::Copying if sending => format!(
                "Sending to {} · {} / {}",
                item.peer,
                size(item.bytes),
                size(item.total)
            ),
            Phase::Copying => format!(
                "Receiving from {} · {} / {}",
                item.peer,
                size(item.bytes),
                size(item.total)
            ),
            Phase::Ready if sending => format!("Ready to paste on {}", item.peer),
            Phase::Ready => "Ready to paste here".to_owned(),
            Phase::Dropped => "File dropped".to_owned(),
            Phase::Cancelled => "Transfer cancelled".to_owned(),
            Phase::Failed => "Transfer failed".to_owned(),
        })
}

fn tooltip(view: Option<&View>, limit: usize) -> String {
    let mut lines = vec![format!("{TITLE} — {}", connection(view))];
    if let Some(view) = view {
        let peers = &view.peers;
        if !peers.is_empty() {
            let online = peers.iter().filter(|peer| peer.connected).count();
            lines[0].push_str(&format!(" · {online}/{} peers", peers.len()));
        }
        for status in (0..2).filter_map(|direction| tooltip_status(view, direction, limit)) {
            if units(&lines) + 1 + status.encode_utf16().count()
                <= limit.saturating_sub(PEER_SUMMARY_SPACE)
            {
                lines.push(status);
            }
        }
        for (index, peer) in peers.iter().enumerate() {
            let text = peer_label(peer);
            let reserve = if index + 1 == peers.len() {
                0
            } else {
                PEER_SUMMARY_SPACE
            };
            if units(&lines) + 1 + text.encode_utf16().count() > limit.saturating_sub(reserve) {
                lines.push(format!("+{} more peers", peers.len() - index));
                break;
            }
            lines.push(text);
        }
    }
    lines.join("\n")
}

fn tooltip_status(view: &View, direction: usize, limit: usize) -> Option<String> {
    let detail = clipboard_status(view, direction)?;
    if limit == usize::MAX || detail.encode_utf16().count() <= 50 {
        return Some(detail);
    }
    if let Some(item) = &view.transfers[direction] {
        if item.phase == Phase::Copying {
            let action = if direction == 0 {
                "Sending"
            } else {
                "Receiving"
            };
            return Some(format!(
                "{action} {} / {}",
                size(item.bytes),
                size(item.total)
            ));
        }
        if item.phase == Phase::Ready && direction == 0 {
            return Some("Ready to paste on peer".to_owned());
        }
    }
    Some(detail)
}

fn units(lines: &[String]) -> usize {
    lines
        .iter()
        .map(|line| line.encode_utf16().count())
        .sum::<usize>()
        + lines.len()
        - 1
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1_000.0),
        _ => format!("{:.2} MB", bytes as f64 / 1_000_000.0),
    }
}

struct TransferDisplay {
    last: Option<(u64, Phase, Instant)>,
    cancelled: bool,
}

impl TransferDisplay {
    fn new() -> Self {
        Self {
            last: None,
            cancelled: false,
        }
    }

    fn observe(&mut self, transfer: Option<&Transfer>) -> Option<(bool, bool)> {
        let transfer = transfer?;
        let changed = self
            .last
            .is_none_or(|(id, phase, _)| id != transfer.id || phase != transfer.phase);
        let new_id = self.last.is_none_or(|(id, _, _)| id != transfer.id);
        if new_id {
            self.cancelled = false;
        }
        if changed {
            self.last = Some((transfer.id, transfer.phase, Instant::now()));
        }
        let visible = transfer.phase == Phase::Copying
            || self
                .last
                .is_some_and(|(_, _, at)| at.elapsed() < Duration::from_secs(3));
        Some((visible, changed))
    }

    fn cancel(&mut self, send: &dyn Fn(Command)) {
        if !self.cancelled
            && let Some((id, Phase::Copying, _)) = self.last
        {
            self.cancelled = true;
            send(Command::Cancel(id));
        }
    }
}
