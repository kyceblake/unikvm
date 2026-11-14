#![allow(unsafe_code)]

use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, UpdateWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSORINFO, CreateCursor, CreateWindowExW, DefWindowProcW, DestroyCursor, DestroyWindow,
    GetCursorInfo, HCURSOR, HWND_TOPMOST, LWA_ALPHA, RegisterClassW, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetCursor, SetLayeredWindowAttributes, SetPhysicalCursorPos, SetWindowPos,
    UnregisterClassW, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, WindowFromPhysicalPoint,
};

const WINDOW_RADIUS: i32 = 4;
const WINDOW_SIZE: i32 = WINDOW_RADIUS * 2 + 1;

/// Owns the cursor at its parked position without activating a window.
pub(super) struct ParkedCursor {
    window: HWND,
    instance: HINSTANCE,
    class: Vec<u16>,
    blank: HCURSOR,
    previous: HCURSOR,
}

impl ParkedCursor {
    pub(super) fn hide_idle() -> io::Result<Self> {
        Self::hide(desktop_cursor()?.ptScreenPos)
    }

    pub(super) fn hide(anchor: POINT) -> io::Result<Self> {
        let class: Vec<_> = "unikvm parked cursor".encode_utf16().chain([0]).collect();
        let previous = desktop_cursor()?.hCursor;
        // SAFETY: The cursor masks each contain 32 rows of 32 bits. Set AND and
        // clear XOR bits leave every pixel transparent.
        unsafe {
            let instance = GetModuleHandleW(null());
            let blank = CreateCursor(
                instance,
                0,
                0,
                32,
                32,
                [255u8; 128].as_ptr().cast(),
                [0u8; 128].as_ptr().cast(),
            );
            if blank.is_null() {
                return Err(io::Error::last_os_error());
            }
            if RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: instance,
                hCursor: blank,
                hbrBackground: GetStockObject(BLACK_BRUSH),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            }) == 0
            {
                let error = io::Error::last_os_error();
                DestroyCursor(blank);
                return Err(error);
            }
            let mut cursor = Self {
                window: null_mut(),
                instance,
                class,
                blank,
                previous,
            };
            cursor.window = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                cursor.class.as_ptr(),
                null(),
                WS_POPUP,
                anchor.x - WINDOW_RADIUS,
                anchor.y - WINDOW_RADIUS,
                WINDOW_SIZE,
                WINDOW_SIZE,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            // Nonzero alpha keeps the window eligible for cursor hit testing.
            if cursor.window.is_null()
                || SetLayeredWindowAttributes(cursor.window, 0, 1, LWA_ALPHA) == 0
                || SetWindowPos(
                    cursor.window,
                    HWND_TOPMOST,
                    anchor.x - WINDOW_RADIUS,
                    anchor.y - WINDOW_RADIUS,
                    WINDOW_SIZE,
                    WINDOW_SIZE,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            UpdateWindow(cursor.window);
            if WindowFromPhysicalPoint(anchor) == cursor.window {
                // Refresh cursor ownership without moving the pointer.
                SetPhysicalCursorPos(anchor.x, anchor.y);
            }
            SetCursor(cursor.blank);
            Ok(cursor)
        }
    }
}

impl Drop for ParkedCursor {
    fn drop(&mut self) {
        // SAFETY: The window and class belong to the current thread. Restore
        // the prior cursor if Windows still has our cursor or cleared it.
        unsafe {
            if !self.window.is_null() {
                DestroyWindow(self.window);
            }
            if let Ok(info) = desktop_cursor()
                && (info.hCursor == self.blank || info.hCursor.is_null())
            {
                if !self.previous.is_null() {
                    SetCursor(self.previous);
                }
                // Ask the newly exposed window to apply its cursor at this point.
                SetPhysicalCursorPos(info.ptScreenPos.x, info.ptScreenPos.y);
            }
            UnregisterClassW(self.class.as_ptr(), self.instance);
            DestroyCursor(self.blank);
        }
    }
}

fn desktop_cursor() -> io::Result<CURSORINFO> {
    let mut info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: The output points to initialized storage with the required size.
    if unsafe { GetCursorInfo(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use unikvm_core::geometry::Edge;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GCLP_HCURSOR, GetClassLongPtrW, GetForegroundWindow,
    };

    #[test]
    #[ignore = "requires an interactive Windows desktop and a stationary mouse"]
    fn parked_cursor_hides_and_restores_without_activation() {
        super::super::windows::check_input_desktop().unwrap();
        let layout = super::super::EdgeLayout::primary(Edge::Left).unwrap();
        let capture = super::super::Capture::with_edge(true, Some(layout)).unwrap();
        let POINT { x, y } = desktop_cursor().unwrap().ptScreenPos;
        // SAFETY: Wake the normal cursor at its existing desktop coordinates.
        assert_ne!(unsafe { SetPhysicalCursorPos(x, y) }, 0);
        let before = wait_for(|| {
            let info = desktop_cursor().unwrap();
            (!info.hCursor.is_null()).then_some(info)
        });
        // SAFETY: These queries borrow the current foreground window handle.
        let foreground = unsafe { GetForegroundWindow() };
        capture.renew_session(1, Instant::now() + Duration::from_secs(2), true);
        let cursor = wait_for(|| blank_at(before.ptScreenPos));
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        let height = u16::MAX / 4;
        capture.return_from_edge(height);
        let point = layout.entry(height).unwrap();
        wait_for(|| {
            let info = desktop_cursor().unwrap();
            (info.hCursor != cursor
                && !info.hCursor.is_null()
                && info.ptScreenPos.x == point.x
                && info.ptScreenPos.y == point.y)
                .then_some(())
        });
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        // SAFETY: Restore the original desktop position saved by the test.
        assert_ne!(unsafe { SetPhysicalCursorPos(x, y) }, 0);
    }

    #[track_caller]
    fn wait_for<T>(mut condition: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(result) = condition() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for cursor transition"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop and a stationary mouse"]
    fn receiver_cursor_hides_until_the_next_handoff() {
        super::super::windows::check_input_desktop().unwrap();
        let edge = super::super::EdgeLayout::primary(Edge::Left).unwrap();
        let capture = super::super::Capture::with_edge(true, Some(edge)).unwrap();
        let position = desktop_cursor().unwrap().ptScreenPos;
        // SAFETY: Refresh the normal cursor at its existing screen coordinates.
        assert_ne!(unsafe { SetPhysicalCursorPos(position.x, position.y) }, 0);
        wait_for(|| (!desktop_cursor().unwrap().hCursor.is_null()).then_some(()));
        let activity = capture.shortcuts.borrow().activity;
        capture.control().hide_after_edge_return(activity);
        let blank = wait_for(|| blank_at(position));
        capture.control().show_idle_cursor();
        wait_for(|| {
            let cursor = desktop_cursor().unwrap().hCursor;
            (!cursor.is_null() && cursor != blank).then_some(())
        });
    }

    fn blank_at(position: POINT) -> Option<HCURSOR> {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;

        let mut class = [0; 64];
        // SAFETY: Win32 writes at most the provided class name buffer length.
        let window = unsafe { WindowFromPhysicalPoint(position) };
        let len = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        if len <= 0 || String::from_utf16_lossy(&class[..len as usize]) != "unikvm parked cursor" {
            return None;
        }
        // SAFETY: The hit-tested window handle is borrowed for the class lookup.
        let cursor = unsafe { GetClassLongPtrW(window, GCLP_HCURSOR) } as HCURSOR;
        (!cursor.is_null() && desktop_cursor().unwrap().hCursor == cursor).then_some(cursor)
    }
}
