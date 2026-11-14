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
#[cfg(target_os = "windows")]
use super::windows::run_hooks as run_monitor;

const QUEUE_SIZE: usize = 256;
pub(super) const POLL: Duration = Duration::from_millis(20);
pub(super) const ARM_DELAY: Duration = Duration::from_secs(3);

pub(super) type MonitorWake = Box<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub start: u64,
    pub stop: u64,
    pub cancelled: bool,
    pub edge_height: Option<u16>,
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
    },
}

pub struct Capture {
    commands: watch::Sender<Command>,
    wake: MonitorWake,
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
                let result = run_monitor(
                    receiver,
                    signals,
                    events,
                    stopped,
                    &initialized,
                    allow_send,
                    edge,
                );
                if let Err(error) = result {
                    let _ = initialized.try_send(Err(error));
                }
            })?;
        let wake = ready.recv().map_err(io::Error::other)??;
        Ok(Self {
            commands,
            wake,
            shortcuts,
            events: input,
            worker: Some(worker),
            stop,
        })
    }

    pub fn renew(&self, generation: u64, expires: Instant) {
        self.renew_session(generation, expires, false);
    }

    pub fn renew_session(&self, generation: u64, expires: Instant, edge: bool) {
        let wake = !matches!(*self.commands.borrow(), Command::Remote { generation: current, .. } if current == generation);
        self.commands.send_replace(Command::Remote {
            generation,
            expires,
            edge,
        });
        if wake {
            (self.wake)();
        }
    }

    pub fn local(&self) {
        self.commands.send_replace(Command::Local {
            return_height: None,
        });
        (self.wake)();
    }

    pub fn return_from_edge(&self, height: u16) {
        self.commands.send_replace(Command::Local {
            return_height: Some(height),
        });
        (self.wake)();
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
