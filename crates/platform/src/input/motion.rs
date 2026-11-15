use std::io;
use std::time::{Duration, Instant};

use unikvm_core::input::PointerMotion;

#[cfg(target_os = "macos")]
use super::macos_motion::{FrameTimer, PointerDisplay};
#[cfg(target_os = "windows")]
use super::windows_motion::{FrameTimer, PointerDisplay};

pub struct MotionPacer {
    motion: PointerMotion,
    timer: FrameTimer,
    display: PointerDisplay,
}

impl MotionPacer {
    pub fn new(sink: &super::InputSink) -> io::Result<Self> {
        let mut display = sink.pointer_display();
        display.update();
        Ok(Self {
            motion: PointerMotion::default(),
            timer: FrameTimer::new()?,
            display,
        })
    }

    /// False means the caller should deliver this sample immediately.
    pub fn queue(&mut self, dx: i32, dy: i32) -> bool {
        self.display.update();
        if let Some(period) = self.display.period {
            self.motion.push(dx, dy, Instant::now(), period);
            true
        } else {
            false
        }
    }

    /// Wait for a scheduled movement and report how late the step resumed.
    /// This can be cancelled between network reads.
    pub async fn next(&mut self) -> io::Result<([i32; 2], Duration)> {
        let now = Instant::now();
        let (Some(period), Some(due)) = (self.display.period, self.motion.due(now)) else {
            return std::future::pending().await;
        };
        self.timer.wait_until(due).await?;
        let resumed = Instant::now();
        Ok((
            self.motion.step(resumed, period),
            resumed.saturating_duration_since(due),
        ))
    }

    pub fn take_pending(&mut self) -> [i32; 2] {
        self.motion.take_remaining()
    }

    pub fn display_period(&self) -> Option<Duration> {
        self.display.period
    }
}
