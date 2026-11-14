#![allow(unsafe_code)]

use std::io;

use unikvm_core::geometry::{DisplayRect, Point};
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetPhysicalCursorPos, SetPhysicalCursorPos};

pub(super) fn primary() -> io::Result<DisplayRect> {
    // SAFETY: Temporarily use physical monitor coordinates on the calling thread.
    let info = unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
        // The primary display owns the virtual origin.
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let ok = GetMonitorInfoW(monitor, &mut info);
        if !previous.is_null() {
            SetThreadDpiAwarenessContext(previous);
        }
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        info
    };
    rect(info.rcMonitor).ok_or_else(|| io::Error::other("invalid primary display bounds"))
}

fn rect(bounds: RECT) -> Option<DisplayRect> {
    let width = u32::try_from(i64::from(bounds.right) - i64::from(bounds.left)).ok()?;
    let height = u32::try_from(i64::from(bounds.bottom) - i64::from(bounds.top)).ok()?;
    DisplayRect::new(
        Point {
            x: bounds.left,
            y: bounds.top,
        },
        width.try_into().ok()?,
        height.try_into().ok()?,
    )
}

pub(super) fn has_display(point: Point) -> bool {
    // SAFETY: Use the same physical coordinate space as the pointer and display bounds.
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let monitor = MonitorFromPoint(
            POINT {
                x: point.x,
                y: point.y,
            },
            MONITOR_DEFAULTTONULL,
        );
        if !previous.is_null() {
            SetThreadDpiAwarenessContext(previous);
        }
        !monitor.is_null()
    }
}

pub(super) fn position() -> io::Result<Point> {
    let mut point = POINT::default();
    // SAFETY: The output points to writable storage.
    if unsafe { GetPhysicalCursorPos(&mut point) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Point {
        x: point.x,
        y: point.y,
    })
}

pub(super) fn place(point: Point) -> io::Result<()> {
    // SAFETY: Coordinates are in the active desktop's physical coordinate space.
    if unsafe { SetPhysicalCursorPos(point.x, point.y) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
