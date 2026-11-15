#![allow(unsafe_code)]

use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, DeleteObject,
    EndPaint, FillRect, GetSysColor, GetSysColorBrush, HDC, HFONT, InvalidateRect, LOGFONTW,
    PAINTSTRUCT, SetBkColor, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetMessageW, GetWindowLongPtrW, IDC_ARROW, IsWindow, LoadCursorW, MSG, PostQuitMessage,
    RegisterClassW, SPI_GETCLIENTAREAANIMATION, SW_SHOWNORMAL, SendMessageW, SetTimer,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, SystemParametersInfoW, TranslateMessage,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_PAINT, WM_SETFONT, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD,
    WS_MINIMIZEBOX, WS_SYSMENU, WS_VISIBLE,
};
use windows_sys::core::w;

use super::{HEADINGS, TITLE, View};

const WIDTH: i32 = 420;
const HEIGHT: i32 = 201;
const MARGIN: i32 = 20;
const ROW_TOPS: [i32; 2] = [56, 124];
const TIMER: usize = 1;
const FRAMES: isize = 32;

pub(super) fn show(read: impl Fn() -> View) -> io::Result<()> {
    // SAFETY: The window, static controls, timer and fonts all belong to this UI thread.
    // String and structure pointers are valid for the duration of the synchronous calls.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
        let dpi = GetDpiForSystem();
        let px = |dip| scale(dip, dpi);
        let instance = GetModuleHandleW(null());
        let class = w!("UniKVMClipboardMonitor");
        let spec = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: class,
            ..WNDCLASSW::default()
        };
        if RegisterClassW(&spec) == 0 {
            return Err(io::Error::last_os_error());
        }

        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut bounds = RECT {
            left: 0,
            top: 0,
            right: px(WIDTH),
            bottom: px(HEIGHT),
        };
        if AdjustWindowRectExForDpi(&mut bounds, style, 0, 0, dpi) == 0 {
            return Err(io::Error::last_os_error());
        }
        let regular = Font::new(px(14), 400)?;
        let bold = Font::new(px(15), 600)?;
        let window = Window(CreateWindowExW(
            0,
            class,
            wide(TITLE).as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            null_mut(),
            null_mut(),
            instance,
            null(),
        ));
        if window.0.is_null() {
            return Err(io::Error::last_os_error());
        }
        let label = |text: &str, y, font: &Font| {
            let handle = CreateWindowExW(
                0,
                w!("STATIC"),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE,
                px(MARGIN),
                px(y),
                px(WIDTH - 2 * MARGIN),
                px(21),
                window.0,
                null_mut(),
                instance,
                null(),
            );
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            SendMessageW(handle, WM_SETFONT, font.0 as usize, 0);
            Ok(handle)
        };
        let mut previous = read();
        let connection = label(previous.connection, 20, &regular)?;
        let mut rows = [null_mut(); 2];
        for (index, y) in ROW_TOPS.into_iter().enumerate() {
            label(HEADINGS[index], y, &bold)?;
            rows[index] = label(previous.rows[index].text, y + 24, &regular)?;
        }
        let mut animate = 1i32;
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animate).cast(), 0);
        if SetTimer(window.0, TIMER, 33, None) == 0 {
            return Err(io::Error::last_os_error());
        }
        set_progress(window.0, previous, 0);
        ShowWindow(window.0, SW_SHOWNORMAL);

        let mut frame = 0;
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, null_mut(), 0, 0) {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(()),
                _ => {}
            }
            if message.hwnd == window.0 && message.message == WM_TIMER && message.wParam == TIMER {
                let next = read();
                if previous.connection != next.connection {
                    SetWindowTextW(connection, wide(next.connection).as_ptr());
                }
                for (index, handle) in rows.into_iter().enumerate() {
                    if previous.rows[index].text != next.rows[index].text {
                        SetWindowTextW(handle, wide(next.rows[index].text).as_ptr());
                    }
                }
                if animate != 0 {
                    frame = (frame + 1) % FRAMES;
                }
                set_progress(window.0, next, frame);
                previous = next;
            } else {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: The handle was created on this thread and can only be live here.
        unsafe {
            if IsWindow(self.0) != 0 {
                DestroyWindow(self.0);
            }
        }
    }
}

struct Font(HFONT);

impl Font {
    fn new(height: i32, weight: i32) -> io::Result<Self> {
        let mut spec = LOGFONTW {
            lfHeight: -height,
            lfWeight: weight,
            ..LOGFONTW::default()
        };
        let face = wide("Segoe UI");
        spec.lfFaceName[..face.len()].copy_from_slice(&face);
        // SAFETY: The initialized descriptor is read synchronously.
        let handle = unsafe { CreateFontIndirectW(&spec) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: The window and child controls no longer use this owned font.
        unsafe { DeleteObject(self.0) };
    }
}

fn set_progress(window: HWND, view: View, frame: isize) {
    // Syscall state holds only integer bits and requires no borrowed pointers.
    let busy = isize::from(view.rows[0].copying) | (isize::from(view.rows[1].copying) << 1);
    let state = if busy == 0 { 0 } else { (frame << 2) | busy };
    // SAFETY: Called on the UI thread with a live window.
    unsafe {
        if SetWindowLongPtrW(window, GWLP_USERDATA, state) != state {
            let dpi = GetDpiForWindow(window);
            for y in ROW_TOPS {
                InvalidateRect(window, &track(y, dpi), 0);
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Win32 supplies a live window and message-specific handles. The
    // paint context is valid between BeginPaint and EndPaint on the UI thread.
    unsafe {
        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(window, &mut paint);
                if !dc.is_null() {
                    let state = GetWindowLongPtrW(window, GWLP_USERDATA);
                    let dpi = GetDpiForWindow(window);
                    for (index, y) in ROW_TOPS.into_iter().enumerate() {
                        let bar = track(y, dpi);
                        FillRect(dc, &bar, GetSysColorBrush(COLOR_WINDOW));
                        if state & (1 << index) != 0 {
                            let width = (bar.right - bar.left) / 4;
                            let offset = (bar.right - bar.left - width) * (state >> 2) as i32
                                / (FRAMES as i32 - 1);
                            let moving = RECT {
                                left: bar.left + offset,
                                right: bar.left + offset + width,
                                ..bar
                            };
                            FillRect(dc, &moving, GetSysColorBrush(COLOR_HIGHLIGHT));
                        }
                    }
                    EndPaint(window, &paint);
                }
                0
            }
            WM_CTLCOLORSTATIC => {
                let dc = wparam as HDC;
                SetBkColor(dc, GetSysColor(COLOR_WINDOW));
                SetTextColor(dc, GetSysColor(COLOR_WINDOWTEXT));
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

fn track(y: i32, dpi: u32) -> RECT {
    RECT {
        left: scale(MARGIN, dpi),
        top: scale(y + 47, dpi),
        right: scale(WIDTH - MARGIN, dpi),
        bottom: scale(y + 51, dpi),
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    (value * dpi as i32 + 48) / 96
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
