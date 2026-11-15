#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::io;
use std::mem::ManuallyDrop;
use std::ptr::null_mut;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};
use std::time::{Duration, Instant};

use core_foundation::base::{TCFType, kCFAllocatorDefault};
use core_foundation::mach_port::{CFMachPort, CFMachPortInvalidate, CFMachPortRef};
use core_foundation::runloop::{
    CFRunLoop, CFRunLoopGetCurrent, CFRunLoopSource, CFRunLoopSourceContext, CFRunLoopSourceCreate,
    CFRunLoopSourceSignal, CFRunLoopStop, CFRunLoopWakeUp, kCFRunLoopDefaultMode,
};
use core_foundation::string::CFStringRef;
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    EventField, KeyCode,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use core_graphics::sys::CGEventRef;
use foreign_types::ForeignType;
use tokio::sync::{mpsc, watch};
use unikvm_core::geometry::Point;
use unikvm_core::input::{ButtonState, InputEvent, KeyUsage, PointerButton};

use super::capture::{
    ARM_DELAY, Command, IdleCursor, MonitorOptions, MonitorWake, POLL, PasteShortcut,
    PhysicalMotion, Shortcut, activity, button_state, request_start,
};
use super::edge::EdgeTrigger;
use super::macos::{EVENT_TAG, MacInputSink, key_code};
use super::macos_cursor::BackgroundCursor;
use crate::clipboard::Clipboard;

const EVENT_TYPES: [CGEventType; 14] = {
    use CGEventType::*;
    [
        KeyDown,
        KeyUp,
        FlagsChanged,
        MouseMoved,
        LeftMouseDragged,
        RightMouseDragged,
        OtherMouseDragged,
        LeftMouseDown,
        LeftMouseUp,
        RightMouseDown,
        RightMouseUp,
        OtherMouseDown,
        OtherMouseUp,
        ScrollWheel,
    ]
};
const SCROLL_SCALE: f64 = 120.0;
const MOMENTUM_PHASE: u32 = 123;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        location: CGEventTapLocation,
        placement: CGEventTapPlacement,
        options: CGEventTapOptions,
        mask: u64,
        callback: unsafe extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enabled: bool);
    fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
    fn CGEventSourceKeyState(source: i32, key: u16) -> bool;
    fn CGEventSourceButtonState(source: i32, button: u32) -> bool;
    fn CGEventKeyboardGetUnicodeString(
        event: CGEventRef,
        max_length: std::ffi::c_ulong,
        actual_length: *mut std::ffi::c_ulong,
        utf16: *mut u16,
    );
    fn CGRequestListenEventAccess() -> bool;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

/// Call from a user-invoked permission flow.
pub fn request_monitor_permission() -> bool {
    // SAFETY: The argument-free API requests permission for the current process.
    unsafe { CGRequestListenEventAccess() }
}

struct TapState {
    commands: watch::Receiver<Command>,
    shortcuts: watch::Sender<Shortcut>,
    events: mpsc::Sender<InputEvent>,
    allow_send: bool,
    cursor: Option<CapturedCursor>,
    idle_cursor: IdleCursor,
    chords: [bool; 2],
    swallowed_keys: [bool; 256],
    swallowed_buttons: [bool; 5],
    generation: Option<u64>,
    stopped_generation: Option<u64>,
    arm_until: Instant,
    motion: PhysicalMotion,
    motion_remainder: [f64; 2],
    edge: Option<EdgeTrigger>,
    scroll_remainder: [f64; 2],
    suspended: bool,
    paste: PasteShortcut,
    pasteboard: Option<Clipboard>,
}

/// The native tap can disappear while macOS owns protected keyboard input.
/// Keep its lifetime separate from the monitor/signals used by the receiver.
struct InstalledTap {
    port: CFMachPort,
    source: CFRunLoopSource,
    run_loop: CFRunLoop,
    mode: CFStringRef,
}

impl InstalledTap {
    fn new(
        state: &RefCell<TapState>,
        run_loop: &CFRunLoop,
        mode: CFStringRef,
        keyboard_only: bool,
    ) -> io::Result<Self> {
        let mask = EVENT_TYPES
            .into_iter()
            .filter(|kind| {
                !keyboard_only || matches!(kind, CGEventType::KeyDown | CGEventType::KeyUp)
            })
            .fold(0, |mask, kind| mask | (1u64 << kind as u32));
        // SAFETY: The caller keeps state alive until this tap is invalidated.
        // The nullable disable notifications are handled before wrapping events.
        let raw = unsafe {
            CGEventTapCreate(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                mask,
                tap_callback,
                (state as *const RefCell<TapState>).cast_mut().cast(),
            )
        };
        if raw.is_null() {
            return Err(tap_permission());
        }
        // SAFETY: Quartz transferred an owned Mach port under the create rule.
        let port = unsafe { CFMachPort::wrap_under_create_rule(raw) };
        let source = match port.create_runloop_source(0) {
            Ok(source) => source,
            Err(()) => {
                // SAFETY: No run loop owns a source for this live port yet.
                unsafe { CFMachPortInvalidate(raw) };
                return Err(io::Error::other("create Quartz event tap run loop source"));
            }
        };
        run_loop.add_source(&source, mode);
        // SAFETY: The owned port remains alive for the installed source's lifetime.
        unsafe { CGEventTapEnable(raw, true) };
        Ok(Self {
            port,
            source,
            run_loop: run_loop.clone(),
            mode,
        })
    }

    fn enabled(&self) -> bool {
        // SAFETY: This owned port is valid until Drop invalidates it.
        unsafe { CGEventTapIsEnabled(self.port.as_concrete_TypeRef()) }
    }
}

impl Drop for InstalledTap {
    fn drop(&mut self) {
        self.run_loop.remove_source(&self.source, self.mode);
        // SAFETY: Stop deliveries before releasing the source, port, or callback state.
        unsafe { CFMachPortInvalidate(self.port.as_concrete_TypeRef()) };
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TapAction {
    Suspend,
    Recreate,
    Observe,
    Stop,
}

fn tap_action(was_suspended: bool, protected: bool, enabled: bool) -> TapAction {
    if protected {
        TapAction::Suspend
    } else if was_suspended {
        TapAction::Recreate
    } else if enabled {
        TapAction::Observe
    } else {
        TapAction::Stop
    }
}

struct RunLoopWake {
    run_loop: CFRunLoop,
    source: CFRunLoopSource,
}

// SAFETY: The source is immutable from Rust and only the thread-safe signal
// operation is exposed across threads. Retained CF objects keep both handles live.
unsafe impl Send for RunLoopWake {}
unsafe impl Sync for RunLoopWake {}

impl RunLoopWake {
    fn new(run_loop: CFRunLoop) -> io::Result<Self> {
        extern "C" fn perform(_: *const c_void) {
            // SAFETY: This callback runs within the monitor's active run loop.
            unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
        }

        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform,
        };
        // SAFETY: The context contains a static callback and no borrowed state.
        let raw = unsafe { CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &mut context) };
        if raw.is_null() {
            return Err(io::Error::other("create input command run loop source"));
        }
        // SAFETY: The create call transferred an owning CF reference.
        let source = unsafe { CFRunLoopSource::wrap_under_create_rule(raw) };
        Ok(Self { run_loop, source })
    }

    fn notify(&self) {
        // SAFETY: Both handles are retained. Signaling before wake also covers
        // commands that arrive just before the monitor enters its wait.
        unsafe {
            CFRunLoopSourceSignal(self.source.as_concrete_TypeRef());
            CFRunLoopWakeUp(self.run_loop.as_concrete_TypeRef());
        }
    }
}

pub(super) fn run_tap(
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
    if !MacInputSink::has_permission() {
        return Err(tap_permission());
    }
    let run_loop = CFRunLoop::get_current();
    let wake = Arc::new(RunLoopWake::new(run_loop.clone())?);
    let pasteboard = paste.as_ref().map(|_| Clipboard::new()).transpose()?;
    let state = RefCell::new(TapState {
        commands,
        shortcuts,
        events,
        allow_send,
        cursor: None,
        idle_cursor: IdleCursor::default(),
        chords: [false; 2],
        swallowed_keys: [false; 256],
        swallowed_buttons: [false; 5],
        generation: None,
        stopped_generation: None,
        arm_until: Instant::now(),
        motion: PhysicalMotion::default(),
        motion_remainder: [0.0; 2],
        edge: edge.map(EdgeTrigger::new),
        scroll_remainder: [0.0; 2],
        suspended: false,
        paste: PasteShortcut::new(paste),
        pasteboard,
    });
    // SAFETY: Core Foundation's default mode is a valid static run loop mode.
    let mode = unsafe { kCFRunLoopDefaultMode };
    let protected = secure_input() || super::macos_session::capture_unavailable();
    state.borrow_mut().set_suspended(protected);
    // Do not create a reduced keyboard mask while Secure Input owns the desktop.
    // Recreate the complete tap after unlock instead of just enabling that mask.
    let mut tap = if protected {
        None
    } else {
        Some(InstalledTap::new(&state, &run_loop, mode, keyboard_only)?)
    };
    run_loop.add_source(&wake.source, mode);

    let result: io::Result<()> = (|| {
        let signal = wake.clone();
        initialized
            .send(Ok(Box::new(move || signal.notify())))
            .map_err(|_| io::Error::other("input monitor startup was cancelled"))?;
        let mut last_permission_check = Instant::now();
        while !stop.load(Relaxed) {
            if last_permission_check.elapsed() >= Duration::from_millis(250) {
                if !MacInputSink::has_permission() {
                    return Err(tap_permission());
                }
                last_permission_check = Instant::now();
            }
            let protected = secure_input() || super::macos_session::capture_unavailable();
            let was_suspended = state.borrow().suspended;
            state.borrow_mut().set_suspended(protected);
            match tap_action(
                was_suspended,
                protected,
                tap.as_ref().is_some_and(InstalledTap::enabled),
            ) {
                TapAction::Suspend | TapAction::Observe => {}
                TapAction::Recreate => {
                    // Drop the old port before replacing it; never leave two
                    // filters registered for the same physical event stream.
                    drop(tap.take());
                    tap = Some(InstalledTap::new(&state, &run_loop, mode, keyboard_only)?);
                }
                TapAction::Stop => return Err(io::Error::other("Quartz event tap was disabled")),
            }
            if protected {
                state.borrow_mut().update_suspended();
            } else {
                let mut state = state.borrow_mut();
                let state = &mut *state;
                state
                    .idle_cursor
                    .update(&mut state.commands, &state.shortcuts);
                let command = *state.commands.borrow();
                state.update(command);
            }
            CFRunLoop::run_in_mode(mode, POLL, false);
        }
        Ok(())
    })();
    state.borrow_mut().local();
    state.borrow_mut().idle_cursor.show();
    drop(tap);
    run_loop.remove_source(&wake.source, mode);
    result
}

fn tap_permission() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "grant Accessibility and, if requested, Input Monitoring to the process running unikvm",
    )
}

unsafe extern "C" fn tap_callback(
    _proxy: *mut c_void,
    kind: u32,
    raw: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    // SAFETY: run_tap owns this state for the lifetime of the callback on this thread.
    let cell = unsafe { &*user_info.cast::<RefCell<TapState>>() };
    let Ok(mut state) = cell.try_borrow_mut() else {
        return raw;
    };
    if kind == CGEventType::TapDisabledByTimeout as u32
        || kind == CGEventType::TapDisabledByUserInput as u32
    {
        if secure_input() || super::macos_session::capture_unavailable() {
            state.set_suspended(true);
        } else {
            state.interrupt();
        }
        return raw;
    }
    if raw.is_null() {
        return raw;
    }
    // SAFETY: A non-null event is live for the callback; Quartz retains ownership.
    let event = ManuallyDrop::new(unsafe { CGEvent::from_ptr(raw) });
    let tagged = event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == EVENT_TAG;
    if !tagged && event.get_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID) != 0 {
        return raw;
    }
    if state.suspended || secure_input() || super::macos_session::capture_unavailable() {
        state.set_suspended(true);
        // Never forward or suppress protected physical input. Mouse activity can
        // still reclaim an incoming session when Quartz delivers it to this tap.
        if !tagged {
            state.protected_activity(&event);
        }
        return raw;
    }
    if (tagged && state.paste(&event, true)) || (!tagged && state.capture(&event)) {
        null_mut()
    } else {
        raw
    }
}

impl TapState {
    fn set_suspended(&mut self, suspended: bool) {
        if suspended && !self.suspended {
            self.interrupt();
            // Old shortcut/held-event bookkeeping must not consume a release
            // intended for the lock screen or a new post-unlock interaction.
            self.chords = [false; 2];
            self.swallowed_keys = [false; 256];
            self.swallowed_buttons = [false; 5];
            self.paste.reset();
            self.motion = PhysicalMotion::default();
        }
        self.suspended = suspended;
    }

    fn update_suspended(&mut self) {
        let command = *self.commands.borrow_and_update();
        self.local();
        self.idle_cursor.show();
        if let Command::Remote { generation, .. } = command
            && self.stopped_generation != Some(generation)
        {
            self.generation = Some(generation);
            self.emergency();
        }
        // Showing a receiving cursor is safe while locked. A stale hide command
        // is consumed here so it cannot hide the lock-screen pointer after unlock.
    }

    fn protected_activity(&mut self, event: &CGEvent) {
        use CGEventType::*;
        match event.get_type() {
            MouseMoved | LeftMouseDragged | RightMouseDragged | OtherMouseDragged => {
                let [dx, dy] = [
                    EventField::MOUSE_EVENT_DELTA_X,
                    EventField::MOUSE_EVENT_DELTA_Y,
                ]
                .map(|field| {
                    event
                        .get_double_value_field(field)
                        .clamp(-32768.0, 32768.0)
                        .round() as i32
                });
                if self.motion.update(dx, dy, Instant::now()) {
                    activity(&self.shortcuts);
                }
            }
            ScrollWheel if event.get_integer_value_field(MOMENTUM_PHASE) != 0 => {}
            KeyDown | KeyUp | FlagsChanged | LeftMouseDown | LeftMouseUp | RightMouseDown
            | RightMouseUp | OtherMouseDown | OtherMouseUp | ScrollWheel => {
                activity(&self.shortcuts);
            }
            _ => {}
        }
    }

    fn update(&mut self, command: Command) {
        match command {
            Command::Local { return_height } => self.return_locally(return_height),
            Command::Remote {
                generation,
                expires,
                edge,
            } if self.stopped_generation != Some(generation) => {
                let now = Instant::now();
                if self.generation != Some(generation) {
                    self.generation = Some(generation);
                    self.arm_until = now + ARM_DELAY;
                }
                if (edge && self.edge.is_none())
                    || now >= expires
                    || (self.cursor.is_none() && now >= self.arm_until)
                {
                    self.emergency();
                } else if self.cursor.is_none() && !physical_input_held() {
                    match CapturedCursor::new() {
                        Ok(cursor) => self.cursor = Some(cursor),
                        Err(()) => self.emergency(),
                    }
                }
                // Recheck outside mouse callbacks too: disassociation can lose
                // its effect when the foreground application changes.
                if self.cursor.as_ref().is_some_and(|cursor| !cursor.park()) {
                    self.emergency();
                }
            }
            Command::Remote { .. } | Command::HideAfterEdgeReturn { .. } => {}
        }
    }

    fn interrupt(&mut self) {
        activity(&self.shortcuts);
        self.emergency();
    }

    fn emergency(&mut self) {
        self.local();
        self.idle_cursor.show();
        self.stopped_generation = self.generation;
        self.shortcuts.send_modify(|signal| {
            signal.stop = signal.stop.wrapping_add(1);
            signal.cancelled = true;
        });
    }

    fn local(&mut self) {
        if self.cursor.take().is_some() {
            self.motion = PhysicalMotion::default();
            self.motion_remainder = [0.0; 2];
        }
        self.scroll_remainder = [0.0; 2];
    }

    fn return_locally(&mut self, height: Option<u16>) {
        if let (Some(cursor), Some(height), Some(edge)) = (&mut self.cursor, height, &mut self.edge)
            && let Ok(point) = edge.layout.entry(height)
        {
            // The cursor guard restores its anchor before revealing it.
            cursor.anchor = CGPoint::new(f64::from(point.x), f64::from(point.y));
            edge.rearm();
        }
        self.local();
    }

    fn enqueue(&mut self, event: InputEvent) -> bool {
        if self.events.try_send(event).is_err() {
            self.emergency();
            return false;
        }
        true
    }

    fn capture(&mut self, event: &CGEvent) -> bool {
        use CGEventType::*;
        let command = *self.commands.borrow();
        // The run loop applies mapped returns between events so an old
        // mouse location cannot override the restored anchor.
        if let Command::Local {
            return_height: None,
        } = command
        {
            self.local();
        }
        let kind = event.get_type();
        if !matches!(kind, ScrollWheel) || event.get_integer_value_field(MOMENTUM_PHASE) == 0 {
            self.idle_cursor.show();
        }
        match kind {
            KeyDown | KeyUp | FlagsChanged => {
                activity(&self.shortcuts);
                let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                self.paste(event, false) || self.key(code, kind, event.get_flags())
            }
            MouseMoved | LeftMouseDragged | RightMouseDragged | OtherMouseDragged => {
                let delta = [
                    EventField::MOUSE_EVENT_DELTA_X,
                    EventField::MOUSE_EVENT_DELTA_Y,
                ]
                .map(|field| event.get_double_value_field(field).clamp(-32768.0, 32768.0));
                if let Some(cursor) = &self.cursor {
                    if !cursor.park() {
                        self.emergency();
                        return false;
                    }
                    let [dx, dy] = std::array::from_fn(|axis| {
                        let total = delta[axis] + self.motion_remainder[axis];
                        let step = total as i32;
                        self.motion_remainder[axis] = total - f64::from(step);
                        step
                    });
                    return (dx == 0 && dy == 0)
                        || self.enqueue(InputEvent::PointerMove { dx, dy });
                }
                let [dx, dy] = delta.map(|axis| axis.round() as i32);
                if matches!(kind, MouseMoved)
                    && let Some(edge) = &mut self.edge
                {
                    let location = event.location();
                    let point = Point {
                        x: location.x.floor() as i32,
                        y: location.y.floor() as i32,
                    };
                    let modifiers = CGEventFlags::CGEventFlagControl
                        | CGEventFlags::CGEventFlagShift
                        | CGEventFlags::CGEventFlagAlternate
                        | CGEventFlags::CGEventFlagCommand;
                    if !event.get_flags().intersects(modifiers)
                        && let Some(height) = edge.attempt(point, dx, 0)
                        && !physical_input_held()
                    {
                        request_start(&self.shortcuts, Some(height));
                    }
                }
                if self.motion.update(dx, dy, Instant::now()) {
                    activity(&self.shortcuts);
                }
                false
            }
            LeftMouseDown | LeftMouseUp | RightMouseDown | RightMouseUp | OtherMouseDown
            | OtherMouseUp => {
                activity(&self.shortcuts);
                self.button(event, kind)
            }
            ScrollWheel => {
                // Quartz can continue to emit momentum events after fingers leave the trackpad.
                if event.get_integer_value_field(MOMENTUM_PHASE) == 0 {
                    activity(&self.shortcuts);
                }
                self.cursor.is_some() && self.scroll(event)
            }
            _ => false,
        }
    }

    fn paste(&mut self, event: &CGEvent, tagged: bool) -> bool {
        use CGEventType::{KeyDown, KeyUp};
        let Some(pasteboard) = self.pasteboard.as_ref() else {
            return false;
        };
        let kind = event.get_type();
        if !matches!(kind, KeyDown | KeyUp) {
            return false;
        }
        let local = self.cursor.is_none();
        let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
        self.paste
            .key(code, tagged, matches!(kind, KeyDown), |requests| {
                let modifiers = CGEventFlags::CGEventFlagControl
                    | CGEventFlags::CGEventFlagShift
                    | CGEventFlags::CGEventFlagAlternate
                    | CGEventFlags::CGEventFlagCommand;
                local
                    && (event.get_flags() & modifiers) == CGEventFlags::CGEventFlagCommand
                    && event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) == 0
                    && paste_key(event, code)
                    && requests.intercept(|| pasteboard.sequence())
            })
    }

    fn key(&mut self, code: u16, kind: CGEventType, flags: CGEventFlags) -> bool {
        use CGEventType::{FlagsChanged, KeyDown};
        if self.allow_send && matches!(code, KeyCode::F9 | KeyCode::F10) {
            let index = usize::from(code == KeyCode::F10);
            if self.chords[index] {
                self.chords[index] = matches!(kind, KeyDown);
                return true;
            }
            if matches!(kind, KeyDown)
                && flags.contains(CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagShift)
            {
                self.chords[index] = true;
                if code == KeyCode::F10 {
                    self.emergency();
                } else if self.cursor.is_none() {
                    request_start(&self.shortcuts, None);
                }
                return true;
            }
        }

        let Some(usage) = key_usage(code) else {
            return self.cursor.is_some();
        };
        // Caps Lock changes toggle state clients see, rather than reporting an ordinary up/down pair.
        if matches!(kind, FlagsChanged) && usage == KeyUsage(0x39) {
            return self.cursor.is_some()
                && self.enqueue(InputEvent::Key {
                    usage,
                    state: ButtonState::Pressed,
                })
                && self.enqueue(InputEvent::Key {
                    usage,
                    state: ButtonState::Released,
                });
        }
        let pressed = if matches!(kind, FlagsChanged) {
            let Some(mask) = MODIFIER_MASKS.get(usize::from(usage.0).wrapping_sub(0xe0)) else {
                return self.cursor.is_some();
            };
            flags.bits() & mask != 0
        } else {
            matches!(kind, KeyDown)
        };
        let index = usize::from(usage.0);
        if self.cursor.is_some()
            && self.enqueue(InputEvent::Key {
                usage,
                state: button_state(pressed),
            })
        {
            self.swallowed_keys[index] = pressed;
            return true;
        }
        let swallow = !pressed && self.swallowed_keys[index];
        self.swallowed_keys[index] = false;
        swallow
    }

    fn button(&mut self, event: &CGEvent, kind: CGEventType) -> bool {
        use CGEventType::*;
        let (index, button) = match kind {
            LeftMouseDown | LeftMouseUp => (0, PointerButton::Primary),
            RightMouseDown | RightMouseUp => (1, PointerButton::Secondary),
            _ => match event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER) {
                2 => (2, PointerButton::Middle),
                3 => (3, PointerButton::Back),
                4 => (4, PointerButton::Forward),
                _ => return self.cursor.is_some(),
            },
        };
        let pressed = matches!(kind, LeftMouseDown | RightMouseDown | OtherMouseDown);
        if self.cursor.is_some()
            && self.enqueue(InputEvent::PointerButton {
                button,
                state: button_state(pressed),
            })
        {
            self.swallowed_buttons[index] = pressed;
            return true;
        }
        let swallow = !pressed && self.swallowed_buttons[index];
        self.swallowed_buttons[index] = false;
        swallow
    }

    fn scroll(&mut self, event: &CGEvent) -> bool {
        let axes = [
            -event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2),
            event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1),
        ];
        let [horizontal, vertical] = std::array::from_fn(|axis| {
            let units =
                (axes[axis] * SCROLL_SCALE + self.scroll_remainder[axis]).clamp(-32768.0, 32768.0);
            let whole = units as i32;
            self.scroll_remainder[axis] = units - f64::from(whole);
            whole
        });
        (horizontal == 0 && vertical == 0)
            || self.enqueue(InputEvent::Wheel {
                horizontal,
                vertical,
            })
    }
}

fn paste_key(event: &CGEvent, code: u16) -> bool {
    let mut character = 0;
    let mut length = 0;
    // SAFETY: The event is live and the buffer holds the single code unit requested.
    unsafe { CGEventKeyboardGetUnicodeString(event.as_ptr(), 1, &mut length, &mut character) };
    match (length, character) {
        (1, 0x56 | 0x76) => true,
        (1, 0x41..=0x5a | 0x61..=0x7a) => false,
        _ => code == KeyCode::ANSI_V,
    }
}

struct CapturedCursor {
    source: CGEventSource,
    anchor: CGPoint,
    disassociated: bool,
    _visibility: BackgroundCursor,
}

impl CapturedCursor {
    fn new() -> Result<Self, ()> {
        // Save the anchor before disassociation. An API success does not prove
        // the cursor stays disconnected while this headless agent is sending.
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)?;
        let anchor = CGEvent::new(source.clone())?.location();
        let visibility = BackgroundCursor::hide()?;
        let disassociated = CGDisplay::associate_mouse_and_mouse_cursor_position(false).is_ok();
        let cursor = Self {
            source,
            anchor,
            disassociated,
            _visibility: visibility,
        };
        if !disassociated {
            CGDisplay::warp_mouse_cursor_position(anchor).map_err(|_| ())?;
        }
        Ok(cursor)
    }

    fn park(&self) -> bool {
        let Ok(event) = CGEvent::new(self.source.clone()) else {
            return false;
        };
        let point = event.location();
        // Quartz warps can round fractional coordinates to a display pixel.
        if (point.x - self.anchor.x).abs() <= 1.0 && (point.y - self.anchor.y).abs() <= 1.0 {
            return true;
        }
        CGDisplay::warp_mouse_cursor_position(self.anchor).is_ok()
    }
}

impl Drop for CapturedCursor {
    fn drop(&mut self) {
        // Recenter while still hidden; the visibility guard is dropped afterward.
        let _ = CGDisplay::warp_mouse_cursor_position(self.anchor);
        if self.disassociated {
            let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
        }
    }
}

// Left/right device flags from IOKit's IOLLEvent.h in HID usage order.
const MODIFIER_MASKS: [u64; 8] = [0x1, 0x2, 0x20, 0x8, 0x2000, 0x4, 0x40, 0x10];

fn key_usage(code: u16) -> Option<KeyUsage> {
    (0x04..=0xe7)
        .map(KeyUsage)
        .find(|&usage| key_code(usage) == Some(code))
}

pub(crate) fn secure_input() -> bool {
    // SAFETY: This argument-free API reads the system Secure Event Input state.
    unsafe { IsSecureEventInputEnabled() != 0 }
}

pub(super) fn physical_input_held() -> bool {
    let source = CGEventSourceStateID::HIDSystemState as i32;
    // SAFETY: Read current hardware state with valid Quartz virtual key and button numbers.
    (0..=127)
        .filter(|&key| key != KeyCode::CAPS_LOCK)
        .any(|key| unsafe { CGEventSourceKeyState(source, key) })
        || (0..5).any(|button| unsafe { CGEventSourceButtonState(source, button) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::EdgeLayout;

    #[test]
    fn protected_desktop_preserves_monitor_and_recreates_full_tap_on_resume() {
        for enabled in [false, true] {
            assert_eq!(tap_action(false, true, enabled), TapAction::Suspend);
            assert_eq!(tap_action(true, true, enabled), TapAction::Suspend);
            assert_eq!(tap_action(true, false, enabled), TapAction::Recreate);
        }
    }

    #[test]
    fn disabled_tap_outside_protected_transition_stays_fail_closed() {
        assert_eq!(tap_action(false, false, false), TapAction::Stop);
        assert_eq!(tap_action(false, false, true), TapAction::Observe);
    }

    struct RestorePointer(CGPoint);

    impl Drop for RestorePointer {
        fn drop(&mut self) {
            let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
            let _ = CGDisplay::warp_mouse_cursor_position(self.0);
        }
    }

    #[test]
    #[ignore = "requires Accessibility and a stationary mouse on the desktop"]
    fn receiver_cursor_hides_until_the_next_handoff() {
        use super::super::macos_cursor::is_visible;
        use unikvm_core::geometry::Edge;

        assert!(is_visible());
        let edge = EdgeLayout::primary(Edge::Right).unwrap();
        let capture = super::super::Capture::with_edge(true, Some(edge)).unwrap();
        let activity = capture.shortcuts.borrow().activity;
        capture.control().hide_after_edge_return(activity);
        let wait_for_visibility = |visible| {
            let deadline = Instant::now() + Duration::from_secs(1);
            while is_visible() != visible {
                assert!(
                    Instant::now() < deadline,
                    "cursor visibility did not change"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        wait_for_visibility(false);
        capture.control().show_idle_cursor();
        wait_for_visibility(true);
    }

    #[test]
    #[ignore = "requires Accessibility and moves the desktop pointer"]
    fn cursor_parking_recovers_native_drift() {
        assert!(MacInputSink::has_permission());
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap();
        let position = || CGEvent::new(source.clone()).unwrap().location();
        let anchor = position();
        let _restore = RestorePointer(anchor);
        let (displays, count) = CGDisplay::displays_with_point(anchor, 1).unwrap();
        assert!(count > 0);
        let bounds = CGDisplay::new(displays[0]).bounds();
        let dx = if anchor.x + 24.0 < bounds.origin.x + bounds.size.width {
            24.0
        } else {
            -24.0
        };
        let drift = CGPoint::new(anchor.x + dx, anchor.y);
        let visible_before = super::super::macos_cursor::is_visible();
        let cursor = CapturedCursor::new().unwrap();
        assert!(!super::super::macos_cursor::is_visible());
        // Even a successful disassociation does not prevent other cursor warps.
        // The lock must detect and correct real drift, regardless of API status.
        for _ in 0..3 {
            CGDisplay::warp_mouse_cursor_position(drift).unwrap();
            let moved = position();
            assert!((moved.x - anchor.x).abs() > 12.0);
            assert!(cursor.park());
            let parked = position();
            assert!((parked.x - anchor.x).abs() <= 1.0);
            assert!((parked.y - anchor.y).abs() <= 1.0);
        }
        drop(cursor);
        assert_eq!(super::super::macos_cursor::is_visible(), visible_before);
    }
}
