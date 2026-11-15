#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromPoint,
};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CloseThreadpoolWait, CreateThreadpoolWait,
    CreateWaitableTimerExW, PTP_CALLBACK_INSTANCE, PTP_WAIT, SYNCHRONIZATION_SYNCHRONIZE,
    SetThreadpoolWait, SetWaitableTimer, TIMER_MODIFY_STATE, WaitForThreadpoolWaitCallbacks,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetPhysicalCursorPos;

#[derive(Default)]
pub(super) struct PointerDisplay {
    monitor: HMONITOR,
    pub(super) period: Option<Duration>,
    checked: Option<Instant>,
}

impl PointerDisplay {
    pub(super) fn update(&mut self) {
        let now = Instant::now();
        let monitor = pointer_monitor();
        if monitor != self.monitor
            || self
                .checked
                .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(250))
        {
            self.monitor = monitor;
            self.period = refresh_period(monitor);
            self.checked = Some(now);
        }
    }
}

fn pointer_monitor() -> HMONITOR {
    // SAFETY: The temporary DPI context applies to this thread and is restored
    // before returning. The cursor API writes to the owned POINT.
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut point = POINT::default();
        let monitor = if GetPhysicalCursorPos(&mut point) != 0 {
            MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST)
        } else {
            null_mut()
        };
        if !previous.is_null() {
            SetThreadDpiAwarenessContext(previous);
        }
        monitor
    }
}

fn refresh_period(monitor: HMONITOR) -> Option<Duration> {
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: cbSize requests MONITORINFOEXW. Its device name is a null-terminated
    // array on success. mode has the required size and no private driver data.
    unsafe {
        if GetMonitorInfoW(monitor, &mut info.monitorInfo) == 0
            || EnumDisplaySettingsW(info.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) == 0
            || mode.dmDisplayFrequency <= 1
        {
            return None;
        }
    }
    Some(Duration::from_secs_f64(
        1.0 / f64::from(mode.dmDisplayFrequency),
    ))
}

pub(super) struct FrameTimer {
    handle: OwnedHandle,
    wait: PTP_WAIT,
    ready: Arc<Notify>,
    armed: Option<Instant>,
}

impl FrameTimer {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: No name or security attributes are provided. The returned
        // non-null handle is owned by this timer.
        let raw = unsafe {
            CreateWaitableTimerExW(
                null(),
                null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                SYNCHRONIZATION_SYNCHRONIZE | TIMER_MODIFY_STATE,
            )
        };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let ready = Arc::new(Notify::new());
        // SAFETY: The Notify stays live until the wait and its callbacks are closed.
        let context = Arc::as_ptr(&ready).cast_mut().cast();
        let wait = unsafe { CreateThreadpoolWait(Some(timer_ready), context, null()) };
        if wait == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            wait,
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
                let relative_100ns = -(delay.as_nanos().div_ceil(100) as i64);
                let raw = self.handle.as_raw_handle();
                // SAFETY: These are owned live handles. The timer consumes the
                // relative deadline during the call; callback state remains live.
                unsafe {
                    if SetWaitableTimer(raw, &relative_100ns, 0, None, null(), 0) == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    SetThreadpoolWait(self.wait, raw, null());
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
        // SAFETY: Stop queueing callbacks and wait for those already running
        // before the timer handle and callback context can be dropped.
        unsafe {
            SetThreadpoolWait(self.wait, null_mut(), null());
            WaitForThreadpoolWaitCallbacks(self.wait, 1);
            CloseThreadpoolWait(self.wait);
        }
    }
}

unsafe extern "system" fn timer_ready(
    _instance: PTP_CALLBACK_INSTANCE,
    context: *mut c_void,
    _wait: PTP_WAIT,
    _result: u32,
) {
    // SAFETY: The wait's context points to a live Notify until all callbacks finish.
    unsafe { &*context.cast::<Notify>() }.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn high_resolution_timer_wakes_for_successive_240_hz_deadlines() {
        let mut timer = FrameTimer::new().unwrap();
        let period = Duration::from_secs_f64(1.0 / 240.0);
        let start = Instant::now();
        for index in 1..=8 {
            let due = start + period * index;
            tokio::time::timeout(Duration::from_secs(1), timer.wait_until(due))
                .await
                .unwrap()
                .unwrap();
            assert!(Instant::now() >= due);
        }
    }

    #[tokio::test]
    #[ignore = "requires an interactive Windows input desktop"]
    async fn paces_injected_steps_to_the_current_monitor() {
        use crate::input::{InputSink, MotionPacer};
        use unikvm_core::input::InputEvent;
        use windows_sys::Win32::UI::WindowsAndMessaging::SetPhysicalCursorPos;

        struct RestoreCursor(POINT);
        impl Drop for RestoreCursor {
            fn drop(&mut self) {
                // SAFETY: Restore the point read from this desktop before the probe.
                unsafe { SetPhysicalCursorPos(self.0.x, self.0.y) };
            }
        }

        let mut original = POINT::default();
        // SAFETY: The cursor API writes to the owned POINT.
        assert_ne!(unsafe { GetPhysicalCursorPos(&mut original) }, 0);
        let _restore = RestoreCursor(original);
        let mut sink = InputSink::new().unwrap();
        let mut pacer = MotionPacer::new(&sink).unwrap();
        let period = refresh_period(pointer_monitor()).expect("monitor refresh rate");
        let mut monitor = MONITORINFOEXW::default();
        monitor.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: The monitor structure and cursor point belong to this desktop.
        let ready = unsafe { GetMonitorInfoW(pointer_monitor(), &mut monitor.monitorInfo) };
        assert_ne!(ready, 0);
        let bounds = monitor.monitorInfo.rcMonitor;
        // SAFETY: Place the cursor inside its current monitor for a short movement test.
        assert_ne!(
            unsafe {
                SetPhysicalCursorPos(
                    (bounds.left + bounds.right) / 2,
                    (bounds.top + bounds.bottom) / 2,
                )
            },
            0
        );
        for _ in 0..2 {
            assert!(pacer.queue(12, 0));
            let ([dx, dy], _) = pacer.next().await.unwrap();
            assert!(dx > 0 && dy == 0);
            sink.send(InputEvent::PointerMove { dx, dy }).unwrap();
        }
        let mut after = POINT::default();
        // SAFETY: The cursor API writes to the owned POINT.
        assert_ne!(unsafe { GetPhysicalCursorPos(&mut after) }, 0);
        assert!(after.x > (bounds.left + bounds.right) / 2);
        println!(
            "Windows monitor reports {:.0} Hz. Two paced steps moved the cursor.",
            1.0 / period.as_secs_f64()
        );
    }
}
