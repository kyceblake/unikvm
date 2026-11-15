use anyhow::{Context, Result, bail};
use tokio::sync::watch;
use unikvm_config::{InputConfig, InputMode, PeerSide};
use unikvm_network::{PeerEndpoint, identity::Fingerprint};

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod receive;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod send;

pub struct Control {
    connections: watch::Receiver<Option<quinn::Connection>>,
    peer: Fingerprint,
    mode: InputMode,
    side: Option<PeerSide>,
    allow_locked_receive: bool,
    file_drag: bool,
}

impl Control {
    pub fn new(config: &InputConfig, endpoint: Option<&PeerEndpoint>) -> Result<Self> {
        if !cfg!(any(target_os = "windows", target_os = "macos")) {
            bail!("input sharing is available on Windows and macOS");
        }
        let peer = config.peer_fingerprint.parse()?;
        let connections = endpoint
            .and_then(|endpoint| endpoint.connection(peer))
            .context("input peer must be configured in the network trust set")?;
        Ok(Self {
            connections,
            peer,
            mode: config.mode,
            side: config.peer_side,
            allow_locked_receive: config.allow_locked_receive,
            file_drag: config.file_drag,
        })
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub async fn run(
        self,
        paste: Option<unikvm_platform::clipboard::PasteRequests>,
        desktop: crate::desktop::Desktop,
    ) -> Result<()> {
        use tracing::info;
        use unikvm_core::geometry::Edge;
        use unikvm_platform::input::{Capture, EdgeLayout};

        let focus = Cell::new(FocusState::Local);
        let allow_send = self.mode != InputMode::Receive;
        let edge = self
            .side
            .map(|side| {
                EdgeLayout::primary(match side {
                    PeerSide::Left => Edge::Left,
                    PeerSide::Right => Edge::Right,
                })
            })
            .transpose()
            .context("read primary display for edge switching")?;
        let mut capture =
            Capture::with_paste(allow_send, edge, paste).context("start local input monitor")?;
        let native = self.file_drag.then(|| desktop.file_drop.clone());
        let (local_drag, _source) = if self.file_drag
            && let Some(edge) = edge
        {
            let route = send::DragSender {
                starts: desktop.drag_starts.subscribe(),
                offers: desktop.drag_offers.clone(),
                returns: (cfg!(windows)).then(|| desktop.file_drop.clone()),
            };
            let side = match edge.side {
                Edge::Left => "--left",
                _ => "--right",
            };
            match crate::desktop::Helper::source(side) {
                Ok(source) => (Some(route), Some(source)),
                Err(_) => {
                    tracing::warn!("could not open the native drag source helper");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        if let Some(edge) = edge {
            info!(side = ?edge.side, "primary display edge switching enabled");
        }
        if self.allow_locked_receive {
            info!("locked-desktop reception enabled for configured input peer");
        }
        if allow_send {
            info!(
                "input ready: Ctrl+Shift+F9 sends to the peer and Ctrl+Shift+F10 returns locally"
            );
        } else {
            info!("ready to receive peer input");
        }
        match self.mode {
            InputMode::Send => {
                send::run(
                    &mut capture,
                    self.connections,
                    self.peer,
                    &focus,
                    (None, None),
                )
                .await
            }
            InputMode::Receive => {
                receive::run(
                    capture.control().clone(),
                    capture.shortcuts.clone(),
                    self.connections,
                    self.peer,
                    &focus,
                    receive::Options {
                        edge: None,
                        allow_locked_receive: self.allow_locked_receive,
                        native: None,
                        remote_drag: None,
                    },
                )
                .await
            }
            InputMode::Bidirectional => {
                let control = capture.control().clone();
                let signals = capture.shortcuts.clone();
                tokio::select! {
                    result = send::run(&mut capture, self.connections.clone(), self.peer, &focus, (edge.map(|edge| edge.side), local_drag)) => result,
                    result = receive::run(control, signals, self.connections, self.peer, &focus, receive::Options { edge, allow_locked_receive: self.allow_locked_receive, native, remote_drag: (cfg!(target_os = "macos") && self.file_drag).then_some(desktop) }) => result,
                }
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub async fn run(self) -> Result<()> {
        let _ = (
            self.connections,
            self.peer,
            self.mode,
            self.side,
            self.allow_locked_receive,
            self.file_drag,
        );
        bail!("input sharing is available on Windows and macOS")
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
use {
    std::cell::Cell,
    unikvm_core::focus::{ControlSession, FocusEvent, FocusState},
};

#[cfg(any(target_os = "macos", target_os = "windows", test))]
struct Focus<'a> {
    state: &'a Cell<FocusState>,
    session: ControlSession,
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
impl<'a> Focus<'a> {
    fn reserve(
        state: &'a Cell<FocusState>,
        session: ControlSession,
        direction: FocusEvent,
    ) -> Option<Self> {
        if state.get() != FocusState::Local {
            return None;
        }
        let next = state.get().transition(direction);
        debug_assert_ne!(next, FocusState::Local);
        state.set(next);
        Some(Self { state, session })
    }

    fn transition(&self, event: FocusEvent) -> FocusState {
        let next = self.state.get().transition(event);
        self.state.set(next);
        next
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
impl Drop for Focus<'_> {
    fn drop(&mut self) {
        self.transition(FocusEvent::EndSession(self.session));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unikvm_core::{device::DeviceId, focus::SessionId};

    #[test]
    fn simultaneous_requests_reserve_each_local_focus_and_neither_can_receive() {
        let left = Cell::new(FocusState::Local);
        let right = Cell::new(FocusState::Local);
        let session = ControlSession {
            peer: DeviceId([2; 16]),
            id: SessionId(1),
        };
        let first = Focus::reserve(&left, session, FocusEvent::Request(session)).unwrap();
        let second = Focus::reserve(&right, session, FocusEvent::Request(session)).unwrap();
        assert!(Focus::reserve(&left, session, FocusEvent::AcceptIncoming(session)).is_none());
        assert!(Focus::reserve(&right, session, FocusEvent::AcceptIncoming(session)).is_none());
        drop((first, second));

        let sending = Focus::reserve(&right, session, FocusEvent::Request(session)).unwrap();
        let receiving =
            Focus::reserve(&left, session, FocusEvent::AcceptIncoming(session)).unwrap();
        assert_eq!(
            sending.transition(FocusEvent::Grant(session)),
            FocusState::Sending(session)
        );
        assert_eq!(
            receiving.transition(FocusEvent::LocalActivity),
            FocusState::Local
        );
        drop((receiving, sending));
        assert_eq!(left.get(), FocusState::Local);
        assert_eq!(right.get(), FocusState::Local);
    }
}
