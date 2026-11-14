use std::time::{Duration, Instant};

/// Distributes relative pointer motion over receiving display frames. It keeps a
/// short window of already received motion and never predicts future movement.
#[derive(Default)]
pub struct PointerMotion {
    remaining: [i32; 2],
    frames: u32,
    last_sample: Option<Instant>,
    next_frame: Option<Instant>,
}

const MAX_SAMPLE_INTERVAL: Duration = Duration::from_millis(32);

impl PointerMotion {
    pub fn push(&mut self, dx: i32, dy: i32, now: Instant, frame_period: Duration) {
        let elapsed = self
            .last_sample
            .replace(now)
            .map(|last| now.duration_since(last));
        let sample_period = elapsed
            .filter(|&duration| duration <= MAX_SAMPLE_INTERVAL)
            .unwrap_or(frame_period);
        let period = frame_period.as_nanos().max(1);
        let frames = ((sample_period.as_nanos() + period / 2) / period).max(1) as u32;
        self.remaining[0] = self.remaining[0].saturating_add(dx);
        self.remaining[1] = self.remaining[1].saturating_add(dy);
        self.frames = self.frames.max(frames);
        // Re-evaluate the next slot against the current monitor's frame period.
        self.next_frame = self.next_frame.map(|last| last.min(now + frame_period));
    }

    pub fn due(&self, now: Instant) -> Option<Instant> {
        (self.remaining != [0, 0]).then(|| self.next_frame.unwrap_or(now))
    }

    pub fn step(&mut self, now: Instant, frame_period: Duration) -> [i32; 2] {
        let divisor = i64::from(self.frames.max(1));
        let step = self.remaining.map(|delta| {
            // Round near the middle of each step so slow sub-frame movement starts promptly.
            let delta = i64::from(delta);
            ((delta + delta.signum() * (divisor / 2)) / divisor) as i32
        });
        for (remaining, delta) in self.remaining.iter_mut().zip(step) {
            *remaining -= delta;
        }
        self.frames = if self.remaining == [0, 0] {
            0
        } else {
            self.frames.saturating_sub(1)
        };
        // Keep the frame cadence after small timer delays. Skip missed slots
        // after a stall so the cursor does not replay them in a burst.
        self.next_frame = self
            .next_frame
            .map(|scheduled| scheduled + frame_period)
            .filter(|&next| next > now && next <= now + frame_period)
            .or(Some(now + frame_period));
        step
    }

    pub fn take_remaining(&mut self) -> [i32; 2] {
        self.frames = 0;
        std::mem::take(&mut self.remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_sixty_hz_samples_over_240_hz_frames_and_preserves_signed_distance() {
        let frame = Duration::from_secs_f64(1.0 / 240.0);
        let start = Instant::now();
        let mut motion = PointerMotion::default();
        motion.push(5, 5, start, frame);
        assert_eq!(motion.step(start, frame), [5, 5]);

        let now = start + frame * 4;
        motion.push(21, -13, now, frame);
        let mut total = [0, 0];
        for i in 0..4 {
            let tick = now + frame * i;
            assert!(motion.due(tick).unwrap() <= tick);
            let [x, y] = motion.step(tick, frame);
            assert!(x > 0 && y < 0);
            total[0] += x;
            total[1] += y;
        }
        assert_eq!(total, [21, -13]);
        assert!(motion.due(now).is_none());
    }

    #[test]
    fn the_first_sample_after_idle_is_immediate_and_motion_can_be_drained_before_a_click() {
        let frame = Duration::from_secs_f64(1.0 / 240.0);
        let start = Instant::now();
        let mut motion = PointerMotion::default();
        motion.push(5, 2, start, frame);
        assert_eq!(motion.step(start, frame), [5, 2]);
        let next = start + frame * 4;
        motion.push(16, -8, next, frame);
        assert_eq!(motion.step(next, frame), [4, -2]);
        assert_eq!(motion.take_remaining(), [12, -6]);
        assert!(motion.due(next).is_none());

        let resumed = next + Duration::from_secs(1);
        motion.push(5, -5, resumed, frame);
        assert!(motion.due(resumed).unwrap() <= resumed);
        assert_eq!(motion.step(resumed, frame), [5, -5]);
    }

    #[test]
    fn combines_faster_input_between_sixty_hz_updates() {
        let sample = Duration::from_secs_f64(1.0 / 240.0);
        let frame = Duration::from_secs_f64(1.0 / 60.0);
        let start = Instant::now();
        let mut motion = PointerMotion::default();
        motion.push(2, -3, start, frame);
        assert_eq!(motion.step(start, frame), [2, -3]);

        for index in 1..=4 {
            let now = start + sample * index;
            motion.push(2, -3, now, frame);
            assert_eq!(motion.due(now), Some(start + frame));
        }
        assert_eq!(motion.step(start + frame, frame), [8, -12]);
        assert!(motion.due(start + frame).is_none());
    }
}
