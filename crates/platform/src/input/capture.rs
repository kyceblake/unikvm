use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use unikvm_core::input::{ButtonState, InputEvent};

use super::EdgeLayout;
#[cfg(target_os = "macos")]
use super::macos_capture::run_tap as run_monitor;
#[cfg(target_os = "macos")]
use super::macos_cursor::BackgroundCursor;
#[cfg(target_os = "windows")]
use super::windows::run_hooks as run_monitor;
#[cfg(target_os = "windows")]
use super::windows_cursor::ParkedCursor;
use crate::clipboard::PasteRequests;

const QUEUE_SIZE: usize = 256;
pub(super) const POLL: Duration = Duration::from_millis(20);
pub(super) const ARM_DELAY: Duration = Duration::from_secs(3);

pub(super) type MonitorWake = Box<dyn Fn() + Send + Sync>;

pub(super) struct MonitorOptions {
    pub allow_send: bool,
    pub edge: Option<EdgeLayout>,
    pub paste: Option<PasteRequests>,
    pub keyboard_only: bool,
}

#[derive(Default)]
pub(super) struct PasteShortcut {
    requests: Option<PasteRequests>,
    held: [Option<u16>; 2],
}

impl PasteShortcut {
    pub fn new(requests: Option<PasteRequests>) -> Self {
        Self {
            requests,
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        self.held = [None; 2];
    }

    pub fn key(
        &mut self,
        code: u16,
        tagged: bool,
        pressed: bool,
        request: impl FnOnce(&PasteRequests) -> bool,
    ) -> bool {
        let held = &mut self.held[usize::from(tagged)];
        if *held == Some(code) {
            if !pressed {
                *held = None;
            }
            return true;
        }
        if pressed && held.is_none() && self.requests.as_ref().is_some_and(request) {
            *held = Some(code);
            return true;
        }
        false
    }
}

#[cfg(target_os = "macos")]
type IdleCursorGuard = BackgroundCursor;
#[cfg(target_os = "windows")]
type IdleCursorGuard = ParkedCursor;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub start: u64,
    pub stop: u64,
    pub cancelled: bool,
    pub edge_height: Option<u16>,
    pub drag_generation: Option<u64>,
    pub drag_epoch: u64,
    /// Changes without recording the content of local activity.
    pub activity: u64,
}

#[derive(Clone, Copy)]
pub(super) enum Command {
    Local {
        return_height: Option<u16>,
    },
    Remote {
        generation: u64,
        expires: Instant,
        edge: bool,
        drag: Option<u64>,
    },
    HideAfterEdgeReturn {
        activity: u64,
    },
}

#[derive(Clone)]
pub struct CaptureControl {
    commands: watch::Sender<Command>,
    wake: Arc<MonitorWake>,
}

pub struct Capture {
    control: CaptureControl,
    pub shortcuts: watch::Receiver<Shortcut>,
    pub events: mpsc::Receiver<InputEvent>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Capture {
    pub fn start(allow_send: bool) -> io::Result<Self> {
        Self::with_edge(allow_send, None)
    }

    pub fn with_edge(allow_send: bool, edge: Option<EdgeLayout>) -> io::Result<Self> {
        Self::with_paste(allow_send, edge, None)
    }

    pub fn for_paste(paste: PasteRequests) -> io::Result<Self> {
        Self::monitor(MonitorOptions {
            allow_send: false,
            edge: None,
            paste: Some(paste),
            keyboard_only: true,
        })
    }

    pub fn with_paste(
        allow_send: bool,
        edge: Option<EdgeLayout>,
        paste: Option<PasteRequests>,
    ) -> io::Result<Self> {
        Self::monitor(MonitorOptions {
            allow_send,
            edge,
            paste,
            keyboard_only: false,
        })
    }

    fn monitor(options: MonitorOptions) -> io::Result<Self> {
        let (commands, receiver) = watch::channel(Command::Local {
            return_height: None,
        });
        let (signals, shortcuts) = watch::channel(Shortcut::default());
        let (events, input) = mpsc::channel(QUEUE_SIZE);
        let (initialized, ready) = std::sync::mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::Builder::new()
            .name("unikvm input monitor".into())
            .spawn(move || {
                let result = run_monitor(receiver, signals, events, stopped, &initialized, options);
                if let Err(error) = result {
                    let _ = initialized.try_send(Err(error));
                }
            })?;
        let wake = ready.recv().map_err(io::Error::other)??;
        Ok(Self {
            control: CaptureControl {
                commands,
                wake: Arc::new(wake),
            },
            shortcuts,
            events: input,
            worker: Some(worker),
            stop,
        })
    }

    pub fn control(&self) -> &CaptureControl {
        &self.control
    }

    pub fn renew(&self, generation: u64, expires: Instant) {
        self.renew_session(generation, expires, false);
    }

    pub fn renew_session(&self, generation: u64, expires: Instant, edge: bool) {
        self.renew_capture(generation, expires, edge, None);
    }

    pub fn renew_drag(&self, generation: u64, expires: Instant, epoch: u64) {
        self.renew_capture(generation, expires, true, Some(epoch));
    }

    pub fn drag_height(&self, side: unikvm_core::geometry::Edge) -> Option<u16> {
        #[cfg(target_os = "macos")]
        return super::macos_capture::drag_height(side);
        #[cfg(target_os = "windows")]
        {
            let _ = side;
            None
        }
    }

    fn renew_capture(&self, generation: u64, expires: Instant, edge: bool, drag: Option<u64>) {
        let previous = self.control.commands.send_replace(Command::Remote {
            generation,
            expires,
            edge,
            drag,
        });
        if !matches!(previous, Command::Remote { generation: current, .. } if current == generation)
        {
            (self.control.wake)();
        }
    }

    pub fn local(&self) {
        self.control.commands.send_replace(Command::Local {
            return_height: None,
        });
        (self.control.wake)();
    }

    pub fn return_from_edge(&self, height: u16) {
        self.control.commands.send_replace(Command::Local {
            return_height: Some(height),
        });
        (self.control.wake)();
    }
}

impl CaptureControl {
    pub fn hide_after_edge_return(&self, activity: u64) {
        if self.commands.send_if_modified(|command| {
            if matches!(command, Command::Remote { .. }) {
                return false;
            }
            *command = Command::HideAfterEdgeReturn { activity };
            true
        }) {
            (self.wake)();
        }
    }

    /// Reveal a parked receiving cursor without interrupting outgoing capture.
    pub fn show_idle_cursor(&self) {
        if self.commands.send_if_modified(|command| {
            if !matches!(command, Command::HideAfterEdgeReturn { .. }) {
                return false;
            }
            *command = Command::Local {
                return_height: None,
            };
            true
        }) {
            (self.wake)();
        }
    }
}

#[derive(Default)]
pub(super) struct IdleCursor(Option<IdleCursorGuard>);

impl IdleCursor {
    pub(super) fn update(
        &mut self,
        commands: &mut watch::Receiver<Command>,
        signals: &watch::Sender<Shortcut>,
    ) {
        if !commands.has_changed().unwrap_or(true) {
            return;
        }
        let command = *commands.borrow_and_update();
        let should_hide = matches!(
            command,
            Command::HideAfterEdgeReturn { activity } if signals.borrow().activity == activity
        );
        if !should_hide {
            self.show();
        } else if self.0.is_none() {
            self.0 = IdleCursorGuard::hide_idle().ok();
        }
    }

    pub(super) fn show(&mut self) {
        self.0 = None;
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.local();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn request_start(signals: &watch::Sender<Shortcut>, edge_height: Option<u16>) {
    signals.send_modify(|signal| {
        signal.start = signal.start.wrapping_add(1);
        signal.cancelled = false;
        signal.edge_height = edge_height;
    });
}

#[derive(Default)]
pub(super) struct PhysicalMotion {
    x: i32,
    y: i32,
    last: Option<Instant>,
}

impl PhysicalMotion {
    pub(super) fn update(&mut self, dx: i32, dy: i32, now: Instant) -> bool {
        if dx == 0 && dy == 0 {
            return false;
        }
        // Group small physical deltas while ignoring an isolated nudge.
        if self
            .last
            .is_none_or(|last| now.duration_since(last) > Duration::from_millis(150))
        {
            self.x = 0;
            self.y = 0;
        }
        self.last = Some(now);
        self.x = self.x.saturating_add(dx);
        self.y = self.y.saturating_add(dy);
        if self.x.unsigned_abs().saturating_add(self.y.unsigned_abs()) < 6 {
            return false;
        }
        *self = Self::default();
        true
    }
}

pub(super) fn activity(signals: &watch::Sender<Shortcut>) {
    signals.send_modify(|signal| signal.activity = signal.activity.wrapping_add(1));
}

pub(super) fn button_state(pressed: bool) -> ButtonState {
    if pressed {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_motion_ignores_jitter_and_accumulates_deliberate_movement() {
        let now = Instant::now();
        let mut motion = PhysicalMotion::default();
        assert!(!motion.update(3, 0, now));
        assert!(!motion.update(-3, 0, now));
        assert!(!motion.update(4, 0, now));
        let later = now + Duration::from_millis(151);
        assert!(!motion.update(4, 0, later));
        assert!(motion.update(1, 1, later));
        assert!(!motion.update(1, 0, later));
    }
}
