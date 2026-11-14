#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::ptr::{NonNull, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_graphics::display::CGDisplay;
use tokio::sync::Notify;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVDisplayLinkCreateWithCGDisplay(display: u32, link: *mut *mut c_void) -> i32;
    fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link: *mut c_void) -> CVTime;
    fn CVDisplayLinkRelease(link: *mut c_void);
}

#[repr(C)]
struct CVTime {
    value: i64,
    scale: i32,
    flags: i32,
}

pub(super) struct PointerDisplay {
    display: u32,
    pub(super) period: Option<Duration>,
    checked: Instant,
}

impl PointerDisplay {
    pub(super) fn new(display: u32) -> Self {
        Self {
            display,
            period: refresh_period(display),
            checked: Instant::now(),
        }
    }

    pub(super) fn update(&mut self) {
        if self.checked.elapsed() >= Duration::from_secs(1) {
            self.period = refresh_period(self.display);
            self.checked = Instant::now();
        }
    }
}

fn refresh_period(display: u32) -> Option<Duration> {
    if let Some(mode) = CGDisplay::new(display).display_mode() {
        let hz = mode.refresh_rate();
        if hz.is_finite() && hz >= 1.0 {
            return Duration::try_from_secs_f64(hz.recip())
                .ok()
                .filter(|period| !period.is_zero());
        }
    }

    // Some display modes omit a fixed refresh rate. Ask Core Video for the
    // nominal period of this display without starting a display-link callback.
    let mut link = null_mut();
    // SAFETY: Core Video writes to the owned pointer and returns a retained link.
    unsafe {
        if CVDisplayLinkCreateWithCGDisplay(display, &mut link) != 0 || link.is_null() {
            return None;
        }
        let time = CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link);
        CVDisplayLinkRelease(link);
        const INDEFINITE: i32 = 1;
        if time.flags & INDEFINITE != 0 || time.value <= 0 || time.scale <= 0 {
            return None;
        }
        Duration::try_from_secs_f64(time.value as f64 / f64::from(time.scale))
            .ok()
            .filter(|period| !period.is_zero())
    }
}

// Apple dispatch sources accept monotonic nanosecond deadlines. Callbacks only
// wake the agent; input injection stays on the agent's thread.
unsafe extern "C" {
    #[link_name = "_dispatch_source_type_timer"]
    static TIMER_SOURCE: u8;
    fn dispatch_source_create(
        kind: *const c_void,
        handle: usize,
        mask: usize,
        queue: *mut c_void,
    ) -> *mut c_void;
    fn dispatch_set_context(object: *mut c_void, context: *mut c_void);
    fn dispatch_source_set_event_handler_f(
        source: *mut c_void,
        handler: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_source_set_cancel_handler_f(
        source: *mut c_void,
        handler: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_source_set_timer(source: *mut c_void, start: u64, interval: u64, leeway: u64);
    fn dispatch_time(base: u64, delta: i64) -> u64;
    fn dispatch_activate(object: *mut c_void);
    fn dispatch_source_cancel(source: *mut c_void);
    fn dispatch_release(object: *mut c_void);
}

pub(super) struct FrameTimer {
    source: NonNull<c_void>,
    ready: Arc<Notify>,
    armed: Option<Instant>,
}

impl FrameTimer {
    pub(super) fn new() -> io::Result<Self> {
        const STRICT: usize = 1;
        // SAFETY: The address of the exported timer type is its descriptor.
        // A null queue selects the default global queue. The timer uses no handle.
        let source = unsafe {
            dispatch_source_create((&raw const TIMER_SOURCE).cast(), 0, STRICT, null_mut())
        };
        let source = NonNull::new(source)
            .ok_or_else(|| io::Error::other("could not create macOS frame timer"))?;
        let ready = Arc::new(Notify::new());
        let context = Arc::into_raw(ready.clone()).cast_mut().cast();
        // SAFETY: The cancel handler owns one reference to the context and runs
        // after the last event handler. The new dispatch object is ours to activate.
        unsafe {
            dispatch_set_context(source.as_ptr(), context);
            dispatch_source_set_event_handler_f(source.as_ptr(), timer_ready);
            dispatch_source_set_cancel_handler_f(source.as_ptr(), release_context);
            dispatch_source_set_timer(source.as_ptr(), u64::MAX, u64::MAX, 0);
            dispatch_activate(source.as_ptr());
        }
        Ok(Self {
            source,
            ready,
            armed: None,
        })
    }

    pub(super) async fn wait_until(&mut self, due: Instant) -> io::Result<()> {
        loop {
            let delay = due.saturating_duration_since(Instant::now());
            if delay.is_zero() {
                return Ok(());
            }
            if self.armed != Some(due) {
                let nanos = delay.as_nanos().min(i64::MAX as u128) as i64;
                // SAFETY: The timer source stays live throughout this one-shot arm.
                unsafe {
                    dispatch_source_set_timer(
                        self.source.as_ptr(),
                        dispatch_time(0, nanos),
                        u64::MAX,
                        0,
                    );
                }
                self.armed = Some(due);
            }
            self.ready.notified().await;
            self.armed = None;
        }
    }
}

impl Drop for FrameTimer {
    fn drop(&mut self) {
        // SAFETY: The source holds its own context until cancellation completes.
        // This releases our one retained source reference.
        unsafe {
            dispatch_source_cancel(self.source.as_ptr());
            dispatch_release(self.source.as_ptr());
        }
    }
}

unsafe extern "C" fn timer_ready(context: *mut c_void) {
    // SAFETY: Dispatch passes the retained Notify context to the event handler.
    unsafe { &*context.cast::<Notify>() }.notify_one();
}

unsafe extern "C" fn release_context(context: *mut c_void) {
    // SAFETY: This runs once after all event handlers. Reclaim the retained reference.
    drop(unsafe { Arc::from_raw(context.cast::<Notify>()) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timer_handles_rearming_after_a_cancelled_wait() {
        let mut timer = FrameTimer::new().unwrap();
        let old = Instant::now() + Duration::from_secs(1);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), timer.wait_until(old))
                .await
                .is_err()
        );
        for _ in 0..4 {
            let due = Instant::now() + Duration::from_secs_f64(1.0 / 240.0);
            tokio::time::timeout(Duration::from_millis(250), timer.wait_until(due))
                .await
                .unwrap()
                .unwrap();
            assert!(Instant::now() >= due);
        }
    }
}
