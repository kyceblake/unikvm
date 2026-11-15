use std::time::Duration;

use tokio::time::Instant;
use unikvm_core::input::InputEvent;

const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const PROBE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub(super) enum InputKind {
    Keyboard,
    MouseMotion,
    MouseButtonOrScroll,
}

impl From<InputEvent> for InputKind {
    fn from(event: InputEvent) -> Self {
        match event {
            InputEvent::Key { .. } => Self::Keyboard,
            InputEvent::PointerMove { .. } => Self::MouseMotion,
            InputEvent::PointerButton { .. } | InputEvent::Wheel { .. } => {
                Self::MouseButtonOrScroll
            }
        }
    }
}

impl InputKind {
    const ALL: [Self; 3] = [Self::Keyboard, Self::MouseMotion, Self::MouseButtonOrScroll];

    fn name(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::MouseMotion => "mouse_motion",
            Self::MouseButtonOrScroll => "mouse_button_or_scroll",
        }
    }
}

pub(super) struct Latencies {
    measurement: &'static str,
    since: Instant,
    buckets: [Latency; InputKind::ALL.len()],
}

#[derive(Default)]
struct Latency {
    samples: u64,
    sum_us: u64,
    max_us: u64,
}

impl Latencies {
    pub fn new(measurement: &'static str) -> Self {
        Self {
            measurement,
            since: Instant::now(),
            buckets: Default::default(),
        }
    }

    pub fn record(&mut self, kind: InputKind, started: Instant) {
        let now = Instant::now();
        let elapsed_us = now.duration_since(started).as_micros() as u64;
        let bucket = &mut self.buckets[kind as usize];
        bucket.samples += 1;
        bucket.sum_us += elapsed_us;
        bucket.max_us = bucket.max_us.max(elapsed_us);
        if now.duration_since(self.since) >= REPORT_INTERVAL {
            self.report();
            self.since = now;
        }
    }

    fn report(&mut self) {
        for (kind, bucket) in InputKind::ALL
            .into_iter()
            .zip(std::mem::take(&mut self.buckets))
        {
            if bucket.samples > 0 {
                tracing::info!(
                    target: "unikvm::diagnostics",
                    measurement = self.measurement,
                    input = kind.name(),
                    samples = bucket.samples,
                    mean_us = bucket.sum_us / bucket.samples,
                    max_us = bucket.max_us,
                    "input latency"
                );
            }
        }
    }
}

impl Drop for Latencies {
    fn drop(&mut self) {
        self.report();
    }
}

pub(super) struct SendProbes {
    previous: [Option<Instant>; InputKind::ALL.len()],
    pub latencies: Latencies,
}

impl SendProbes {
    pub fn new() -> Self {
        Self {
            previous: [None; InputKind::ALL.len()],
            latencies: Latencies::new("send_to_peer_ack_rtt"),
        }
    }

    pub fn sample(&mut self, event: InputEvent, now: Instant) -> Option<InputKind> {
        let kind = InputKind::from(event);
        let previous = &mut self.previous[kind as usize];
        if previous.is_some_and(|sent| now.duration_since(sent) < PROBE_INTERVAL) {
            return None;
        }
        *previous = Some(now);
        Some(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unikvm_core::input::{ButtonState, KeyUsage};

    #[test]
    fn keyboard_and_mouse_round_trip_probes_have_separate_rate_limits() {
        let mut probes = SendProbes::new();
        let start = Instant::now();
        let mouse = InputEvent::PointerMove { dx: 1, dy: 0 };
        let key = InputEvent::Key {
            usage: KeyUsage(4),
            state: ButtonState::Pressed,
        };
        assert!(matches!(
            probes.sample(mouse, start),
            Some(InputKind::MouseMotion)
        ));
        assert!(matches!(
            probes.sample(key, start),
            Some(InputKind::Keyboard)
        ));
        let fast = start + Duration::from_millis(999);
        assert!(probes.sample(mouse, fast).is_none());
        assert!(probes.sample(key, fast).is_none());
        assert!(probes.sample(mouse, start + PROBE_INTERVAL).is_some());
    }
}
