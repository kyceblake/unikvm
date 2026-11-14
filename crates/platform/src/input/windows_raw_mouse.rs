#![allow(unsafe_code)]

use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, RegisterClassW, UnregisterClassW,
    WNDCLASSW,
};

pub(super) struct RawMouse {
    window: HWND,
    instance: HINSTANCE,
    class: Vec<u16>,
    registered: bool,
}

impl RawMouse {
    pub(super) fn new() -> io::Result<Self> {
        let class: Vec<_> = "unikvm raw mouse".encode_utf16().chain([0]).collect();
        // SAFETY: Create a private message-only window on the hook thread.
        // The class name and default window procedure stay live while registered.
        unsafe {
            let instance = GetModuleHandleW(null());
            if RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            }) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                instance,
                null(),
            );
            if window.is_null() {
                let error = io::Error::last_os_error();
                UnregisterClassW(class.as_ptr(), instance);
                return Err(error);
            }
            let mouse = RAWINPUTDEVICE {
                usUsagePage: 1,
                usUsage: 2,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: window,
            };
            let mut raw = Self {
                window,
                instance,
                class,
                registered: false,
            };
            if RegisterRawInputDevices(&mouse, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
                return Err(io::Error::last_os_error());
            }
            raw.registered = true;
            Ok(raw)
        }
    }

    pub(super) fn delta(&self, window: HWND, message: LPARAM) -> Option<(i32, i32)> {
        if window != self.window {
            return None;
        }
        let mut input = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        // SAFETY: WM_INPUT carries a live handle. The aligned input buffer is
        // writable, and only a complete mouse record permits reading the union.
        let mouse = unsafe {
            let bytes = GetRawInputData(
                message as HRAWINPUT,
                RID_INPUT,
                (&raw mut input).cast(),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            );
            if bytes != size_of::<RAWINPUT>() as u32
                || input.header.dwType != RIM_TYPEMOUSE
                || input.header.hDevice.is_null()
            {
                return None;
            }
            input.data.mouse
        };
        (mouse.usFlags & MOUSE_MOVE_ABSOLUTE == 0 && (mouse.lLastX != 0 || mouse.lLastY != 0))
            .then_some((mouse.lLastX, mouse.lLastY))
    }
}

impl Drop for RawMouse {
    fn drop(&mut self) {
        let mouse = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: null_mut(),
        };
        // SAFETY: Remove our registration and release this thread's window and class.
        unsafe {
            if self.registered {
                RegisterRawInputDevices(&mouse, 1, size_of::<RAWINPUTDEVICE>() as u32);
            }
            DestroyWindow(self.window);
            UnregisterClassW(self.class.as_ptr(), self.instance);
        }
    }
}
