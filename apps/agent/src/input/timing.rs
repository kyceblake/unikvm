use std::time::Duration;

use tokio::time::Instant;
use unikvm_core::input::InputEvent;
use unikvm_platform::input::NativePostTiming;

const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const BUDGET_US: u64 = 3_000;
const BIN_US: u64 = 250;
const BINS: usize = 129;

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

struct Latency {
    samples: u64,
    sum_us: u64,
    max_us: u64,
    over_budget: u64,
    histogram: [u64; BINS],
    worst: Option<Stages>,
}

#[derive(Clone, Copy)]
enum Stages {
    RoundTrip {
        submit: Duration,
        after_submit: Duration,
        path: quinn::PathStats,
    },
    NativePost {
        before_post: Duration,
        post: Duration,
        pacing: Option<Pacing>,
        native: Option<NativePostTiming>,
    },
}

#[derive(Clone, Copy, Default)]
struct Pacing {
    zero_steps: u32,
    max_late: Duration,
}

pub(super) struct ReceivedSample {
    received: Instant,
    pacing: Option<Pacing>,
}

impl ReceivedSample {
    pub fn new() -> Self {
        Self {
            received: Instant::now(),
            pacing: None,
        }
    }

    pub fn paced_step(&mut self, late_by: Duration, empty: bool) {
        let pacing = self.pacing.get_or_insert_default();
        pacing.zero_steps += u32::from(empty);
        pacing.max_late = pacing.max_late.max(late_by);
    }
}

impl Default for Latency {
    fn default() -> Self {
        Self {
            samples: 0,
            sum_us: 0,
            max_us: 0,
            over_budget: 0,
            histogram: [0; BINS],
            worst: None,
        }
    }
}

impl Latency {
    fn add(&mut self, elapsed: Duration, stages: Stages) {
        let elapsed_us = elapsed.as_nanos().div_ceil(1_000) as u64;
        self.samples += 1;
        self.sum_us += elapsed_us;
        self.over_budget += u64::from(elapsed_us > BUDGET_US);
        let bin = elapsed_us.div_ceil(BIN_US).min((BINS - 1) as u64) as usize;
        self.histogram[bin] += 1;
        if self.worst.is_none() || elapsed_us > self.max_us {
            self.max_us = elapsed_us;
            self.worst = Some(stages);
        }
    }

    fn p95_upper_us(&self) -> u64 {
        let rank = (self.samples * 95).div_ceil(100);
        let mut seen = 0;
        for (index, count) in self.histogram[..BINS - 1].iter().enumerate() {
            seen += count;
            if seen >= rank {
                return (index as u64 * BIN_US).min(self.max_us);
            }
        }
        self.max_us
    }
}

impl Latencies {
    pub fn new(measurement: &'static str) -> Self {
        Self {
            measurement,
            since: Instant::now(),
            buckets: Default::default(),
        }
    }

    pub fn acknowledgment(
        &mut self,
        kind: InputKind,
        sent: Instant,
        submitted: Instant,
        acknowledged: Instant,
        path: quinn::PathStats,
    ) {
        self.record(
            kind,
            sent,
            acknowledged,
            Stages::RoundTrip {
                submit: submitted.duration_since(sent),
                after_submit: acknowledged.duration_since(submitted),
                path,
            },
        );
    }

    pub fn posted(
        &mut self,
        kind: InputKind,
        sample: ReceivedSample,
        posting: Instant,
        native: Option<NativePostTiming>,
    ) {
        let posted = Instant::now();
        self.record(
            kind,
            sample.received,
            posted,
            Stages::NativePost {
                before_post: posting.duration_since(sample.received),
                post: posted.duration_since(posting),
                pacing: sample.pacing,
                native,
            },
        );
    }

    fn record(&mut self, kind: InputKind, started: Instant, ended: Instant, stages: Stages) {
        self.buckets[kind as usize].add(ended.duration_since(started), stages);
        if ended.duration_since(self.since) >= REPORT_INTERVAL {
            self.report();
            self.since = ended;
        }
    }

    fn report(&mut self) {
        for (kind, bucket) in InputKind::ALL
            .into_iter()
            .zip(std::mem::take(&mut self.buckets))
        {
            let Some(worst) = bucket.worst else { continue };
            let (submit, after_submit, path, before_post, post, pacing, native) = match worst {
                Stages::RoundTrip {
                    submit,
                    after_submit,
                    path,
                } => (
                    Some(submit),
                    Some(after_submit),
                    Some(path),
                    None,
                    None,
                    None,
                    None,
                ),
                Stages::NativePost {
                    before_post,
                    post,
                    pacing,
                    native,
                } => (
                    None,
                    None,
                    None,
                    Some(before_post),
                    Some(post),
                    pacing,
                    native,
                ),
            };
            tracing::info!(
                target: "unikvm::diagnostics",
                measurement = self.measurement,
                input = kind.name(),
                samples = bucket.samples,
                mean_us = bucket.sum_us / bucket.samples,
                max_us = bucket.max_us,
                p95_upper_us = bucket.p95_upper_us(),
                budget_us = BUDGET_US,
                over_budget = bucket.over_budget,
                worst_submit_us = submit.map(micros),
                worst_after_submit_us = after_submit.map(micros),
                worst_quic_rtt_us = path.map(|path| micros(path.rtt)),
                worst_quic_min_rtt_us = path.map(|path| micros(path.min_rtt)),
                worst_quic_lost_packets_total = path.map(|path| path.lost_packets),
                worst_before_post_us = before_post.map(micros),
                worst_post_us = post.map(micros),
                worst_zero_steps = pacing.map(|pacing| pacing.zero_steps),
                worst_tick_late_us = pacing.map(|pacing| micros(pacing.max_late)),
                worst_native_access_us = native.map(|native| micros(native.access)),
                worst_native_prepare_us = native.map(|native| micros(native.prepare)),
                worst_native_post_us = native.map(|native| micros(native.post)),
                "input latency"
            );
        }
    }
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros() as u64
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
    fn budget_includes_three_milliseconds_and_details_follow_the_slowest_sample() {
        let mut latency = Latency::default();
        let send = Stages::RoundTrip {
            submit: Duration::from_micros(200),
            after_submit: Duration::from_micros(2_800),
            path: quinn::PathStats::default(),
        };
        for _ in 0..19 {
            latency.add(Duration::from_millis(3), send);
        }
        let mut motion = ReceivedSample::new();
        motion.paced_step(Duration::from_micros(11), true);
        motion.paced_step(Duration::from_micros(31), false);
        let post = Stages::NativePost {
            before_post: Duration::from_micros(2_901),
            post: Duration::from_micros(100),
            pacing: motion.pacing,
            native: None,
        };
        latency.add(Duration::from_micros(3_001), post);
        assert_eq!(latency.over_budget, 1);
        assert_eq!(latency.p95_upper_us(), 3_000);
        latency.add(Duration::from_micros(3_000), send);
        assert_eq!(latency.max_us, 3_001);
        assert!(matches!(
            latency.worst,
            Some(Stages::NativePost { pacing: Some(Pacing { zero_steps: 1, max_late }), .. })
                if max_late == Duration::from_micros(31)
        ));
    }

    #[test]
    fn percentile_uses_the_upper_bucket_and_retains_long_stalls() {
        let mut latency = Latency::default();
        let stages = Stages::RoundTrip {
            submit: Duration::ZERO,
            after_submit: Duration::ZERO,
            path: quinn::PathStats::default(),
        };
        for _ in 0..18 {
            latency.add(Duration::from_millis(4), stages);
        }
        latency.add(Duration::from_micros(5_123), stages);
        latency.add(Duration::from_millis(50), stages);
        assert_eq!(latency.p95_upper_us(), 5_250);
        latency.add(Duration::from_millis(60), stages);
        assert_eq!(latency.p95_upper_us(), 60_000);
    }

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
        let fast = start + PROBE_INTERVAL - Duration::from_millis(1);
        assert!(probes.sample(mouse, fast).is_none());
        assert!(probes.sample(key, fast).is_none());
        assert!(probes.sample(mouse, start + PROBE_INTERVAL).is_some());
    }
}
