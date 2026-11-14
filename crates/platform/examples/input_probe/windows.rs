#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetThreadDesktop, GetUserObjectInformationW, HDESK,
    OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOUSEEVENTF_MOVE, MOUSEINPUT, RegisterHotKey, SendInput,
    UnregisterHotKey, VK_F10, VK_F24,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, HHOOK, HOOKPROC, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, SetCursorPos,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_HOTKEY,
};

const PASS: usize = 0x4b56_4d50;
const BLOCK: usize = 0x4b56_4d42;
const HOTKEY_ID: i32 = 1;
static KEY_SEEN: AtomicU32 = AtomicU32::new(0);
static KEY_PASSED: AtomicU32 = AtomicU32::new(0);
static MOUSE_SEEN: AtomicU32 = AtomicU32::new(0);
static MOUSE_PASSED: AtomicU32 = AtomicU32::new(0);
static PHYSICAL_KEYS: AtomicU32 = AtomicU32::new(0);
static PHYSICAL_MOUSE: AtomicU32 = AtomicU32::new(0);

pub(super) fn run() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let manual = match args.next().as_deref() {
        None => false,
        Some("--manual") if args.next().is_none() => true,
        _ => return Err(io::Error::other("usage: input_probe [--manual]")),
    };

    // SAFETY: This single-threaded process has created no windows.
    check(unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })?;
    check_input_desktop()?;
    let _hotkey = Hotkey::register()?;
    println!("Desktop access and Ctrl+Shift+F10 registration passed.");

    // Install observers first so they sit downstream of the suppression hooks.
    let _keyboard_observer = Hook::install(WH_KEYBOARD_LL, Some(observe_keyboard))?;
    let _mouse_observer = Hook::install(WH_MOUSE_LL, Some(observe_mouse))?;
    let _keyboard_filter = Hook::install(WH_KEYBOARD_LL, Some(filter_keyboard))?;
    let _mouse_filter = Hook::install(WH_MOUSE_LL, Some(filter_mouse))?;

    {
        let original = CursorRestore(cursor_position()?);
        check_edge(original.0)?;
        inject(BLOCK)?;
        pump(Duration::from_millis(500), || {
            KEY_SEEN.load(Relaxed) == 2 && MOUSE_SEEN.load(Relaxed) == 1
        })?;
        if KEY_SEEN.load(Relaxed) != 2
            || MOUSE_SEEN.load(Relaxed) != 1
            || KEY_PASSED.load(Relaxed) != 0
            || MOUSE_PASSED.load(Relaxed) != 0
        {
            return Err(io::Error::other(
                "suppressed events did not match expectations",
            ));
        }
        inject(PASS)?;
        pump(Duration::from_millis(500), || {
            KEY_PASSED.load(Relaxed) == 2 && MOUSE_PASSED.load(Relaxed) == 1
        })?;
        if KEY_SEEN.load(Relaxed) != 4
            || MOUSE_SEEN.load(Relaxed) != 2
            || KEY_PASSED.load(Relaxed) != 2
            || MOUSE_PASSED.load(Relaxed) != 1
        {
            return Err(io::Error::other(
                "forwarded events did not match expectations",
            ));
        }
        println!("Tagged keyboard and mouse injection, capture, and suppression passed.");
    }

    if manual {
        PHYSICAL_KEYS.store(0, Relaxed);
        PHYSICAL_MOUSE.store(0, Relaxed);
        println!("Within 10 seconds, move the mouse and press Ctrl+Shift+F10.");
        let hotkey = pump(Duration::from_secs(10), || false)?;
        let keys = PHYSICAL_KEYS.load(Relaxed);
        let mouse = PHYSICAL_MOUSE.load(Relaxed);
        println!("Physical key events: {keys}. Physical mouse events: {mouse}. Hotkey: {hotkey}.");
        if !hotkey || keys == 0 || mouse == 0 {
            return Err(io::Error::other("manual checks were incomplete"));
        }
    }
    Ok(())
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn desktop_name(desktop: HDESK) -> io::Result<Vec<u16>> {
    let mut name = [0u16; 256];
    let mut bytes = 0;
    // SAFETY: The handle is valid and the buffer has the supplied byte capacity.
    check(unsafe {
        GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr().cast::<c_void>(),
            size_of::<[u16; 256]>() as u32,
            &mut bytes,
        )
    })?;
    Ok(name[..bytes as usize / size_of::<u16>()].to_vec())
}

fn check_input_desktop() -> io::Result<()> {
    // SAFETY: Both queries use this process's desktop context. Only OpenInputDesktop owns a handle.
    let (active, current) = unsafe {
        (
            OpenInputDesktop(0, 0, DESKTOP_READOBJECTS),
            GetThreadDesktop(GetCurrentThreadId()),
        )
    };
    if active.is_null() {
        return Err(io::Error::last_os_error());
    }
    let active_name = desktop_name(active);
    // SAFETY: active is an owned desktop handle and is no longer needed.
    unsafe { CloseDesktop(active) };
    if active_name? != desktop_name(current)? {
        return Err(io::Error::other(
            "the probe is outside the current input desktop",
        ));
    }
    Ok(())
}

struct Hotkey;

impl Hotkey {
    fn register() -> io::Result<Self> {
        // SAFETY: A null window registers the hotkey on this thread's message queue.
        check(unsafe {
            RegisterHotKey(
                null_mut(),
                HOTKEY_ID,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                VK_F10.into(),
            )
        })?;
        Ok(Self)
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        // SAFETY: This is the same thread and registration ID used to register.
        unsafe { UnregisterHotKey(null_mut(), HOTKEY_ID) };
    }
}

struct Hook(HHOOK);

impl Hook {
    fn install(kind: i32, callback: HOOKPROC) -> io::Result<Self> {
        // SAFETY: The callback has the Win32 hook ABI and resides in this executable.
        let hook = unsafe { SetWindowsHookExW(kind, callback, GetModuleHandleW(null_mut()), 0) };
        if hook.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(hook))
        }
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: This instance owns a successfully registered hook.
        unsafe { UnhookWindowsHookEx(self.0) };
    }
}

fn keyboard_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
    count: &AtomicU32,
    filter: bool,
) -> LRESULT {
    if code == 0 {
        // SAFETY: WH_KEYBOARD_LL supplies a valid KBDLLHOOKSTRUCT on HC_ACTION.
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if event.flags & LLKHF_INJECTED == 0 {
            if filter {
                PHYSICAL_KEYS.fetch_add(1, Relaxed);
            }
        } else if event.vkCode == u32::from(VK_F24) && matches!(event.dwExtraInfo, PASS | BLOCK) {
            count.fetch_add(1, Relaxed);
            if filter && event.dwExtraInfo == BLOCK {
                return 1;
            }
        }
    }
    // SAFETY: The hook callback arguments are forwarded unchanged.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn mouse_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
    count: &AtomicU32,
    filter: bool,
) -> LRESULT {
    if code == 0 {
        // SAFETY: WH_MOUSE_LL supplies a valid MSLLHOOKSTRUCT on HC_ACTION.
        let event = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if event.flags & LLMHF_INJECTED == 0 {
            if filter {
                PHYSICAL_MOUSE.fetch_add(1, Relaxed);
            }
        } else if matches!(event.dwExtraInfo, PASS | BLOCK) {
            count.fetch_add(1, Relaxed);
            if filter && event.dwExtraInfo == BLOCK {
                return 1;
            }
        }
    }
    // SAFETY: The hook callback arguments are forwarded unchanged.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn filter_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    keyboard_hook(code, wparam, lparam, &KEY_SEEN, true)
}

unsafe extern "system" fn observe_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    keyboard_hook(code, wparam, lparam, &KEY_PASSED, false)
}

unsafe extern "system" fn filter_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    mouse_hook(code, wparam, lparam, &MOUSE_SEEN, true)
}

unsafe extern "system" fn observe_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    mouse_hook(code, wparam, lparam, &MOUSE_PASSED, false)
}

fn inject(tag: usize) -> io::Result<()> {
    let inputs = [
        keyboard_input(tag, 0),
        keyboard_input(tag, KEYEVENTF_KEYUP),
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: -1,
                    dwFlags: MOUSEEVENTF_MOVE,
                    dwExtraInfo: tag,
                    ..MOUSEINPUT::default()
                },
            },
        },
    ];
    // SAFETY: The array elements match their union discriminant and live across the call.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent == inputs.len() as u32 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if sent == 1 && tag == PASS {
        // SAFETY: Retry the matching key-up once if the key-down alone was accepted.
        unsafe { SendInput(1, &inputs[1], size_of::<INPUT>() as i32) };
    }
    Err(io::Error::other(format!(
        "SendInput accepted {sent} of {} events ({error})",
        inputs.len()
    )))
}

fn keyboard_input(tag: usize, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_F24,
                dwFlags: flags,
                dwExtraInfo: tag,
                ..KEYBDINPUT::default()
            },
        },
    }
}

fn pump(duration: Duration, done: impl Fn() -> bool) -> io::Result<bool> {
    let deadline = Instant::now() + duration;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let mut msg = MSG::default();
        // SAFETY: PeekMessage writes to msg. Inspect it only after a successful dequeue.
        while Instant::now() < deadline
            && unsafe { PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) } != 0
        {
            if msg.message == WM_HOTKEY && msg.wParam == HOTKEY_ID as usize {
                return Ok(true);
            }
            // SAFETY: msg was initialized by PeekMessage.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if done() {
            break;
        }
        // SAFETY: Zero handles and a null handle pointer wait only for the thread's message queue.
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(
                0,
                null_mut(),
                remaining.as_millis() as u32,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if wait == u32::MAX {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(false)
}

fn cursor_position() -> io::Result<POINT> {
    let mut point = POINT::default();
    // SAFETY: The output point is writable.
    check(unsafe { GetCursorPos(&mut point) })?;
    Ok(point)
}

struct CursorRestore(POINT);

impl Drop for CursorRestore {
    fn drop(&mut self) {
        // SAFETY: These are coordinates read from the input desktop before probing.
        unsafe { SetCursorPos(self.0.x, self.0.y) };
    }
}

fn check_edge(point: POINT) -> io::Result<()> {
    // SAFETY: This query takes coordinates by value.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
    if monitor.is_null() {
        return Err(io::Error::other("cursor is outside any monitor"));
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    // SAFETY: The monitor is valid and info has the required size and layout.
    check(unsafe { GetMonitorInfoW(monitor, &mut info) })?;
    let bounds = info.rcMonitor;
    for x in [bounds.right - 2, bounds.right - 1] {
        // SAFETY: x and y are within the current monitor in desktop coordinates.
        check(unsafe { SetCursorPos(x, point.y) })?;
        let actual = cursor_position()?;
        if (actual.x, actual.y) != (x, point.y) {
            return Err(io::Error::other(
                "cursor did not reach the requested coordinate",
            ));
        }
    }
    println!("Cursor warp and right-edge arrival passed.");
    Ok(())
}
