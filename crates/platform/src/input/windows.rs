#![allow(unsafe_code)]

use std::cell::RefCell;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use unikvm_core::input::{InputEvent, KeyUsage, PointerButton};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetThreadDesktop, GetUserObjectInformationW, HDESK,
    OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentThreadId, SetEvent};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_F9, VK_F10, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, HHOOK, HOOKPROC, KBDLLHOOKSTRUCT,
    LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_INPUT, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use super::capture::{
    ARM_DELAY, Command, IdleCursor, MonitorOptions, MonitorWake, POLL, PasteShortcut,
    PhysicalMotion, Shortcut, activity, button_state, request_start,
};
use super::edge::EdgeTrigger;
use super::windows_cursor::ParkedCursor;
use super::windows_raw_mouse::RawMouse;
use super::windows_sink::EVENT_TAG;
use unikvm_core::geometry::Point;

const DESKTOP_CHECK: Duration = Duration::from_millis(250);

thread_local! {
    static HOOK_STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

struct HookState {
    commands: watch::Receiver<Command>,
    shortcuts: watch::Sender<Shortcut>,
    events: mpsc::Sender<InputEvent>,
    remote: bool,
    allow_send: bool,
    chords: [bool; 2],
    down: [bool; 8],
    swallowed_keys: [bool; 256],
    swallowed_buttons: [bool; 5],
    anchor: POINT,
    cursor: Option<ParkedCursor>,
    idle_cursor: IdleCursor,
    raw_motion: bool,
    stopped_generation: Option<u64>,
    generation: Option<u64>,
    arm_until: Instant,
    last_pointer: POINT,
    motion: PhysicalMotion,
    edge: Option<EdgeTrigger>,
    pointer_buttons: [bool; 5],
    paste: PasteShortcut,
}

pub(super) fn run_hooks(
    commands: watch::Receiver<Command>,
    shortcuts: watch::Sender<Shortcut>,
    events: mpsc::Sender<InputEvent>,
    stop: Arc<AtomicBool>,
    initialized: &std::sync::mpsc::SyncSender<io::Result<MonitorWake>>,
    options: MonitorOptions,
) -> io::Result<()> {
    let MonitorOptions {
        allow_send,
        edge,
        paste,
        keyboard_only,
    } = options;
    // SAFETY: This dedicated hook thread owns its DPI context.
    if unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_null()
    {
        return Err(io::Error::last_os_error());
    }
    check_input_desktop()?;
    let raw_mouse = edge.map(|_| RawMouse::new()).transpose()?;
    // SAFETY: Create an unnamed auto-reset event for the command producer.
    let event = unsafe { CreateEventW(null(), 0, 0, null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The successful create transferred ownership of the event handle.
    let wake = Arc::new(unsafe { OwnedHandle::from_raw_handle(event) });
    let mut last_pointer = POINT::default();
    // SAFETY: GetCursorPos writes to this owned POINT.
    if unsafe { GetCursorPos(&mut last_pointer) } == 0 {
        return Err(io::Error::last_os_error());
    }
    HOOK_STATE.with(|state| {
        *state.borrow_mut() = Some(HookState {
            commands,
            shortcuts,
            events,
            remote: false,
            allow_send,
            chords: [false; 2],
            down: [false; 8],
            swallowed_keys: [false; 256],
            swallowed_buttons: [false; 5],
            anchor: POINT::default(),
            cursor: None,
            idle_cursor: IdleCursor::default(),
            raw_motion: false,
            stopped_generation: None,
            generation: None,
            arm_until: Instant::now(),
            last_pointer,
            motion: PhysicalMotion::default(),
            edge: edge.map(EdgeTrigger::new),
            pointer_buttons: [false; 5],
            paste: PasteShortcut::new(paste),
        });
    });
    let _keyboard = Hook::install(WH_KEYBOARD_LL, keyboard_hook)?;
    let _mouse = (!keyboard_only)
        .then(|| Hook::install(WH_MOUSE_LL, mouse_hook))
        .transpose()?;
    let signal = wake.clone();
    initialized
        .send(Ok(Box::new(move || {
            // SAFETY: The owned event handle stays live across this call.
            unsafe { SetEvent(signal.as_raw_handle()) };
        })))
        .map_err(|_| io::Error::other("input monitor startup was cancelled"))?;

    let mut last_desktop_check = Instant::now();
    let mut desktop_available = true;
    let result = loop {
        if stop.load(Relaxed) {
            break Ok(());
        }
        let now = Instant::now();
        let mut desktop_lost = false;
        if now.duration_since(last_desktop_check) >= DESKTOP_CHECK {
            let available = check_input_desktop().is_ok();
            desktop_lost = desktop_available && !available;
            desktop_available = available;
            last_desktop_check = now;
        }
        HOOK_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut().expect("hook state initialized");
            if !desktop_available {
                if desktop_lost {
                    state.paste.reset();
                    activity(&state.shortcuts);
                    state.emergency();
                }
                return;
            }
            state
                .idle_cursor
                .update(&mut state.commands, &state.shortcuts);
            let command = *state.commands.borrow();
            match command {
                Command::Remote {
                    generation,
                    expires,
                    edge,
                } if state.stopped_generation != Some(generation) => {
                    if state.generation != Some(generation) {
                        state.generation = Some(generation);
                        state.arm_until = now + ARM_DELAY;
                    }
                    if now >= expires || (!state.remote && now >= state.arm_until) {
                        state.emergency();
                    } else if !state.remote && !physical_input_held() {
                        if check_input_desktop().is_err() {
                            state.emergency();
                        } else {
                            state.begin_capture(edge);
                        }
                    }
                }
                Command::Local { return_height } => state.local(return_height),
                Command::Remote { .. } | Command::HideAfterEdgeReturn { .. } => {}
            }
        });
        let mut message = MSG::default();
        // Bound each batch so the watchdog also runs under continuous input.
        for _ in 0..64 {
            // SAFETY: These Win32 calls access only this thread's queue and initialized message.
            if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } == 0 {
                break;
            }
            if message.message == WM_INPUT
                && let Some((dx, dy)) = raw_mouse
                    .as_ref()
                    .and_then(|mouse| mouse.delta(message.hwnd, message.lParam))
            {
                HOOK_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    let state = state.as_mut().expect("hook state initialized");
                    state.idle_cursor.show();
                    state.relative_motion(dx, dy);
                });
            }
            unsafe { DispatchMessageW(&message) };
        }
        // SAFETY: Wait for the live event handle or this thread's message queue.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                &wake.as_raw_handle(),
                POLL.as_millis() as u32,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if result == u32::MAX {
            break Err(io::Error::last_os_error());
        }
    };
    HOOK_STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.local(None);
            state.idle_cursor.show();
        }
    });
    result
}

impl HookState {
    fn begin_capture(&mut self, edge: bool) {
        // SAFETY: GetCursorPos writes to this owned POINT. Edge sessions use
        // raw relative motion, so they never need to relocate the cursor.
        if unsafe { GetCursorPos(&mut self.anchor) } == 0 {
            self.emergency();
            return;
        }
        let Ok(cursor) = ParkedCursor::hide(self.anchor) else {
            self.emergency();
            return;
        };
        self.cursor = Some(cursor);
        self.raw_motion = edge;
        self.remote = true;
    }

    fn relative_motion(&mut self, dx: i32, dy: i32) {
        self.check_capture_command();
        if self.remote && self.raw_motion {
            self.enqueue(InputEvent::PointerMove {
                dx: dx.clamp(-32768, 32768),
                dy: dy.clamp(-32768, 32768),
            });
        }
    }

    fn emergency(&mut self) {
        self.local(None);
        self.idle_cursor.show();
        self.stopped_generation = self.generation;
        self.shortcuts.send_modify(|signal| {
            signal.stop = signal.stop.wrapping_add(1);
            signal.cancelled = true;
        });
    }

    fn enqueue(&mut self, event: InputEvent) -> bool {
        if self.events.try_send(event).is_err() {
            self.emergency();
            false
        } else {
            true
        }
    }

    fn key(&mut self, event: &KBDLLHOOKSTRUCT, pressed: bool) -> bool {
        self.check_capture_command();
        // Switching to the Japanese IME can produce an untagged key release
        // during remote injection. A physical press already reclaims control.
        if pressed {
            self.idle_cursor.show();
            activity(&self.shortcuts);
        }
        let usage = key_usage(event.scanCode, event.flags & LLKHF_EXTENDED != 0);
        if let Some(usage @ 0xe0..=0xe7) = usage {
            self.down[usize::from(usage - 0xe0)] = pressed;
        }

        let vk = event.vkCode as u16;
        if self.paste(event, false, pressed) {
            return true;
        }
        if self.allow_send && matches!(vk, VK_F9 | VK_F10) {
            let index = usize::from(vk == VK_F10);
            if self.chords[index] {
                self.chords[index] = pressed;
                return true;
            }
            if pressed && (self.down[0] || self.down[4]) && (self.down[1] || self.down[5]) {
                self.chords[index] = true;
                if vk == VK_F10 {
                    self.emergency();
                } else if !self.remote {
                    request_start(&self.shortcuts, None);
                }
                return true;
            }
        }

        let Some(usage) = usage else {
            return self.remote;
        };
        let index = usize::from(usage);
        if self.remote
            && self.enqueue(InputEvent::Key {
                usage: KeyUsage(usage),
                state: button_state(pressed),
            })
        {
            self.swallowed_keys[index] = pressed;
            return true;
        }
        if !pressed && self.swallowed_keys[index] {
            self.swallowed_keys[index] = false;
            return true;
        }
        if pressed {
            self.swallowed_keys[index] = false;
        }
        false
    }

    fn paste(&mut self, event: &KBDLLHOOKSTRUCT, tagged: bool, pressed: bool) -> bool {
        let local = !self.remote;
        self.paste
            .key(event.scanCode as u16, tagged, pressed, |requests| {
                if !local || event.vkCode != u32::from(VK_V) {
                    return false;
                }
                // SAFETY: Query the state of earlier modifier events, never the V event
                // currently being delivered to the hook.
                let down = |vk| unsafe { GetAsyncKeyState(i32::from(vk)) } < 0;
                down(VK_CONTROL)
                    && !down(VK_V)
                    && ![VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(down)
                    && requests.intercept(|| {
                        // SAFETY: Reads the sequence for the current window station.
                        Ok(u64::from(unsafe { GetClipboardSequenceNumber() }))
                    })
            })
    }

    fn pointer(&mut self, event: &MSLLHOOKSTRUCT, message: u32) -> bool {
        self.check_capture_command();
        let previous = self.last_pointer;
        self.last_pointer = event.pt;
        if event.flags & LLMHF_INJECTED != 0 {
            return false;
        }
        if message != WM_MOUSEMOVE || (event.pt.x, event.pt.y) != (previous.x, previous.y) {
            self.idle_cursor.show();
        }
        if message != WM_MOUSEMOVE
            || (!self.remote
                && self.motion.update(
                    event.pt.x.saturating_sub(previous.x),
                    event.pt.y.saturating_sub(previous.y),
                    Instant::now(),
                ))
        {
            activity(&self.shortcuts);
        }
        if message == WM_MOUSEMOVE && self.remote {
            if self.raw_motion {
                return true;
            }
            let dx = event
                .pt
                .x
                .saturating_sub(self.anchor.x)
                .clamp(-32768, 32768);
            let dy = event
                .pt
                .y
                .saturating_sub(self.anchor.y)
                .clamp(-32768, 32768);
            return (dx == 0 && dy == 0) || self.enqueue(InputEvent::PointerMove { dx, dy });
        }
        if message == WM_MOUSEMOVE
            && !self.down.contains(&true)
            && !self.pointer_buttons.contains(&true)
            && let Some(edge) = &mut self.edge
            && let Some(height) = edge.attempt(
                Point {
                    x: previous.x,
                    y: previous.y,
                },
                event.pt.x.saturating_sub(previous.x),
                event.pt.y.saturating_sub(previous.y),
            )
            && !physical_input_held()
        {
            request_start(&self.shortcuts, Some(height));
        }
        let mouse_data = (event.mouseData >> 16) as u16;
        let mapped = match message {
            WM_LBUTTONDOWN | WM_LBUTTONUP => {
                Some((PointerButton::Primary, 0, message == WM_LBUTTONDOWN))
            }
            WM_RBUTTONDOWN | WM_RBUTTONUP => {
                Some((PointerButton::Secondary, 1, message == WM_RBUTTONDOWN))
            }
            WM_MBUTTONDOWN | WM_MBUTTONUP => {
                Some((PointerButton::Middle, 2, message == WM_MBUTTONDOWN))
            }
            WM_XBUTTONDOWN | WM_XBUTTONUP if mouse_data == 1 => {
                Some((PointerButton::Back, 3, message == WM_XBUTTONDOWN))
            }
            WM_XBUTTONDOWN | WM_XBUTTONUP if mouse_data == 2 => {
                Some((PointerButton::Forward, 4, message == WM_XBUTTONDOWN))
            }
            _ => None,
        };
        if let Some((button, index, pressed)) = mapped {
            self.pointer_buttons[index] = pressed;
            if self.remote
                && self.enqueue(InputEvent::PointerButton {
                    button,
                    state: button_state(pressed),
                })
            {
                self.swallowed_buttons[index] = pressed;
                return true;
            }
            if !pressed && self.swallowed_buttons[index] {
                self.swallowed_buttons[index] = false;
                return true;
            }
            if pressed {
                self.swallowed_buttons[index] = false;
            }
        } else if self.remote && matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
            let delta = i32::from(mouse_data as i16);
            return self.enqueue(InputEvent::Wheel {
                horizontal: if message == WM_MOUSEHWHEEL { delta } else { 0 },
                vertical: if message == WM_MOUSEWHEEL { delta } else { 0 },
            });
        }
        self.remote
    }

    fn check_capture_command(&mut self) {
        let command = *self.commands.borrow();
        // Place the returning cursor between messages. The current mouse
        // hook event may still refer to the parked coordinates.
        if let Command::Local {
            return_height: None,
        } = command
        {
            self.local(None);
        }
    }

    fn local(&mut self, return_height: Option<u16>) {
        self.remote = false;
        if std::mem::take(&mut self.raw_motion)
            && let (Some(height), Some(edge)) = (return_height, &mut self.edge)
            && let Ok(point) = edge.layout.place(height)
        {
            self.last_pointer = POINT {
                x: point.x,
                y: point.y,
            };
            edge.rearm();
        }
        self.cursor = None;
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0
        && matches!(
            wparam as u32,
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP
        )
    {
        // SAFETY: WH_KEYBOARD_LL provides a live KBDLLHOOKSTRUCT for HC_ACTION.
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let pressed = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if HOOK_STATE.with(|state| {
            state
                .try_borrow_mut()
                .ok()
                .and_then(|mut state| {
                    state.as_mut().map(|state| {
                        if event.flags & LLKHF_INJECTED == 0 {
                            state.key(event, pressed)
                        } else if event.dwExtraInfo == EVENT_TAG {
                            state.check_capture_command();
                            state.paste(event, true, pressed)
                        } else {
                            false
                        }
                    })
                })
                .unwrap_or(false)
        }) {
            return 1;
        }
    }
    // SAFETY: Forward the original hook arguments.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 {
        // SAFETY: WH_MOUSE_LL provides a live MSLLHOOKSTRUCT for HC_ACTION.
        let event = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if HOOK_STATE.with(|state| {
            state
                .try_borrow_mut()
                .ok()
                .and_then(|mut state| {
                    state
                        .as_mut()
                        .map(|state| state.pointer(event, wparam as u32))
                })
                .unwrap_or(false)
        }) {
            return 1;
        }
    }
    // SAFETY: Forward the original hook arguments.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

pub(super) fn physical_input_held() -> bool {
    // SAFETY: These argument-only Win32 calls read current key/button state outside a hook.
    (1..=254).any(|key| unsafe { GetAsyncKeyState(key) } < 0)
}

pub(crate) fn check_input_desktop() -> io::Result<()> {
    // SAFETY: Only OpenInputDesktop returns an owned handle.
    let active = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if active.is_null() {
        return Err(io::Error::last_os_error());
    }
    let active_name = desktop_name(active);
    // SAFETY: Close the owned handle after the name query.
    unsafe { CloseDesktop(active) };
    // SAFETY: The thread desktop handle is borrowed from this live thread.
    let current = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    if active_name? != desktop_name(current)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Windows input desktop changed",
        ));
    }
    Ok(())
}

fn desktop_name(desktop: HDESK) -> io::Result<[u16; 256]> {
    let mut name = [0u16; 256];
    let mut written = 0;
    // SAFETY: The array is writable and its byte size is passed to Win32.
    if unsafe {
        GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            std::mem::size_of_val(&name) as u32,
            &mut written,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(name)
}

struct Hook(HHOOK);

impl Hook {
    fn install(
        kind: i32,
        callback: unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT,
    ) -> io::Result<Self> {
        let callback: HOOKPROC = Some(callback);
        // SAFETY: The callback has the hook ABI and resides in this executable.
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
        // SAFETY: This guard owns the installed hook.
        unsafe { UnhookWindowsHookEx(self.0) };
    }
}

pub(super) fn key_usage(scan: u32, extended: bool) -> Option<u16> {
    // Set 1 scan codes map to physical positions, keeping layout interpretation on the Mac.
    const MAIN: [u8; 89] = [
        0, 0x29, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2d, 0x2e, 0x2a,
        0x2b, 0x14, 0x1a, 0x08, 0x15, 0x17, 0x1c, 0x18, 0x0c, 0x12, 0x13, 0x2f, 0x30, 0x28, 0xe0,
        0x04, 0x16, 0x07, 0x09, 0x0a, 0x0b, 0x0d, 0x0e, 0x0f, 0x33, 0x34, 0x35, 0xe1, 0x31, 0x1d,
        0x1b, 0x06, 0x19, 0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0xe5, 0x55, 0xe2, 0x2c, 0x39, 0x3a,
        0x3b, 0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0x41, 0x42, 0x43, 0x53, 0x47, 0x5f, 0x60, 0x61, 0x56,
        0x5c, 0x5d, 0x5e, 0x57, 0x59, 0x5a, 0x5b, 0x62, 0x63, 0, 0, 0x64, 0x44, 0x45,
    ];
    let usage = if extended {
        match scan {
            0x1c => 0x58,
            0x1d => 0xe4,
            0x35 => 0x54,
            0x38 => 0xe6,
            0x47 => 0x4a,
            0x48 => 0x52,
            0x49 => 0x4b,
            0x4b => 0x50,
            0x4d => 0x4f,
            0x4f => 0x4d,
            0x50 => 0x51,
            0x51 => 0x4e,
            0x52 => 0x49,
            0x53 => 0x4c,
            0x5b => 0xe3,
            0x5c => 0xe7,
            _ => 0,
        }
    } else {
        *MAIN.get(scan as usize).unwrap_or(&0)
    };
    (usage != 0).then_some(u16::from(usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook_state(
        shortcuts: watch::Sender<Shortcut>,
        events: mpsc::Sender<InputEvent>,
    ) -> HookState {
        HookState {
            commands: watch::channel(Command::Local {
                return_height: None,
            })
            .1,
            shortcuts,
            events,
            remote: false,
            allow_send: true,
            chords: [false; 2],
            down: [false; 8],
            swallowed_keys: [false; 256],
            swallowed_buttons: [false; 5],
            anchor: POINT::default(),
            cursor: None,
            idle_cursor: IdleCursor::default(),
            raw_motion: false,
            stopped_generation: None,
            generation: None,
            arm_until: Instant::now(),
            last_pointer: POINT::default(),
            motion: PhysicalMotion::default(),
            edge: None,
            pointer_buttons: [false; 5],
            paste: PasteShortcut::default(),
        }
    }

    #[test]
    fn translates_physical_positions_and_distinguishes_extended_keys() {
        assert_eq!(key_usage(0x1e, false), Some(0x04));
        assert_eq!(key_usage(0x1d, false), Some(0xe0));
        assert_eq!(key_usage(0x1d, true), Some(0xe4));
        assert_eq!(key_usage(0x50, false), Some(0x5a));
        assert_eq!(key_usage(0x50, true), Some(0x51));
        assert_eq!(key_usage(0xff, false), None);
    }

    #[test]
    #[ignore = "requires an interactive Windows input desktop"]
    fn capture_thread_installs_and_releases_hooks() {
        let capture = super::super::Capture::start(true).unwrap();
        assert_eq!(capture.shortcuts.borrow().start, 0);
        drop(capture);
        let capture = super::super::Capture::start(false).unwrap();
        assert_eq!(capture.shortcuts.borrow().start, 0);
    }

    #[test]
    #[ignore = "requires an interactive Windows input desktop"]
    fn clipboard_offer_intercepts_tagged_paste_only_for_the_current_copy() {
        use crate::clipboard::{Clipboard, PasteRequests};
        use crate::input::{Capture, InputSink};
        use std::sync::atomic::AtomicUsize;
        use std::thread;

        static FORWARDED: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "system" fn guard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            if code == 0 {
                // SAFETY: WH_KEYBOARD_LL supplies a live KBDLLHOOKSTRUCT for HC_ACTION.
                let key = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
                if key.dwExtraInfo == EVENT_TAG && key.vkCode == u32::from(VK_V) {
                    FORWARDED.fetch_add(1, Relaxed);
                    // Keep the probe from pasting into the foreground app.
                    return 1;
                }
            }
            // SAFETY: Forward the original hook arguments.
            unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
        }

        let _guard = Hook::install(WH_KEYBOARD_LL, guard).unwrap();
        let requests = PasteRequests::default();
        let _capture = Capture::for_paste(requests.clone()).unwrap();
        let sequence = Clipboard::new().unwrap().sequence().unwrap();
        for (token, expected, forwarded) in [(1, sequence, 0), (2, sequence + 1, 2)] {
            requests.arm(token, expected, Instant::now() + Duration::from_secs(5));
            let injector = thread::spawn(|| -> io::Result<()> {
                let mut sink = InputSink::new()?;
                for (usage, pressed) in [(0xe0, true), (0x19, true), (0x19, false), (0xe0, false)] {
                    sink.send(InputEvent::Key {
                        usage: KeyUsage(usage),
                        state: button_state(pressed),
                    })?;
                }
                Ok(())
            });

            let deadline = Instant::now() + Duration::from_secs(2);
            while !injector.is_finished() && Instant::now() < deadline {
                let mut message = MSG::default();
                // SAFETY: This test thread owns the queue and writes into an owned MSG.
                unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) };
                thread::sleep(Duration::from_millis(1));
            }
            assert!(
                injector.is_finished(),
                "input hook queue stopped responding"
            );
            injector.join().unwrap().unwrap();
            assert_eq!(requests.requested(token), token == 1);
            assert_eq!(FORWARDED.load(Relaxed), forwarded);
        }
    }

    #[test]
    fn start_and_return_shortcuts_do_not_forward_the_function_keys() {
        let (shortcuts, state) = tokio::sync::watch::channel(Default::default());
        let (events, mut captured) = tokio::sync::mpsc::channel(4);
        let mut hooks = hook_state(shortcuts, events);
        let key = |vk, scan| KBDLLHOOKSTRUCT {
            vkCode: vk,
            scanCode: scan,
            ..Default::default()
        };
        assert!(!hooks.key(&key(0xa2, 0x1d), true));
        assert!(!hooks.key(&key(0xa0, 0x2a), true));
        assert!(hooks.key(&key(u32::from(VK_F9), 0x43), true));
        assert_eq!(state.borrow().start, 1);
        assert!(hooks.key(&key(u32::from(VK_F9), 0x43), false));
        assert!(hooks.key(&key(u32::from(VK_F10), 0x44), true));
        assert!(state.borrow().cancelled);
        assert!(hooks.key(&key(u32::from(VK_F10), 0x44), false));
        assert!(captured.try_recv().is_err());
        assert!(!hooks.remote);
    }

    #[test]
    fn an_untagged_key_release_does_not_reclaim_local_control() {
        let (shortcuts, state) = tokio::sync::watch::channel(Default::default());
        let (events, _) = tokio::sync::mpsc::channel(4);
        let mut hooks = hook_state(shortcuts, events);
        let key = KBDLLHOOKSTRUCT {
            vkCode: 0x5b,
            scanCode: 0x5b,
            flags: super::LLKHF_EXTENDED,
            ..Default::default()
        };
        assert!(!hooks.key(&key, false));
        assert_eq!(state.borrow().activity, 0);
        assert!(!hooks.key(&key, true));
        assert_eq!(state.borrow().activity, 1);
        assert!(!hooks.key(&key, false));
        assert_eq!(state.borrow().activity, 1);
    }

    #[test]
    fn edge_capture_suppresses_legacy_motion_and_forwards_relative_motion_once() {
        let (shortcuts, _) = watch::channel(Shortcut::default());
        let (events, mut captured) = mpsc::channel(4);
        let mut hooks = hook_state(shortcuts, events);
        let (_commands, receiver) = watch::channel(Command::Remote {
            generation: 1,
            expires: Instant::now() + Duration::from_secs(1),
            edge: true,
        });
        hooks.commands = receiver;
        hooks.remote = true;
        hooks.raw_motion = true;
        let mouse = MSLLHOOKSTRUCT {
            pt: POINT { x: 0, y: 230 },
            ..Default::default()
        };
        assert!(hooks.pointer(&mouse, WM_MOUSEMOVE));
        assert!(captured.try_recv().is_err());
        hooks.relative_motion(-4, 2);
        assert_eq!(
            captured.try_recv().unwrap(),
            InputEvent::PointerMove { dx: -4, dy: 2 }
        );
        assert!(captured.try_recv().is_err());
    }
}
