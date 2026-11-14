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

use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortInvalidate, CFMachPortRef};
use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
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

use super::EdgeLayout;
use super::capture::{
    ARM_DELAY, Command, POLL, PhysicalMotion, Shortcut, activity, button_state, request_start,
};
use super::edge::EdgeTrigger;
use super::macos::{EVENT_TAG, MacInputSink, key_code};
use super::macos_cursor::BackgroundCursor;

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
}

pub(super) fn run_tap(
    commands: watch::Receiver<Command>,
    shortcuts: watch::Sender<Shortcut>,
    events: mpsc::Sender<InputEvent>,
    stop: Arc<AtomicBool>,
    initialized: &std::sync::mpsc::SyncSender<io::Result<()>>,
    allow_send: bool,
    edge: Option<EdgeLayout>,
) -> io::Result<()> {
    if !MacInputSink::has_permission() {
        return Err(tap_permission());
    }
    let state = RefCell::new(TapState {
        commands,
        shortcuts,
        events,
        allow_send,
        cursor: None,
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
    });
    let mask = EVENT_TYPES
        .into_iter()
        .fold(0, |mask, kind| mask | (1u64 << kind as u32));
    // SAFETY: The state outlives the tap and its run loop source. Only this thread dispatches it.
    // Use the native callback so disable notifications can carry a null CGEventRef.
    let raw = unsafe {
        CGEventTapCreate(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            tap_callback,
            (&raw const state).cast_mut().cast(),
        )
    };
    if raw.is_null() {
        return Err(tap_permission());
    }
    // SAFETY: Quartz returned an owned Mach port under the create rule.
    let tap = unsafe { CFMachPort::wrap_under_create_rule(raw) };
    let source = tap
        .create_runloop_source(0)
        .map_err(|()| io::Error::other("create Quartz event tap run loop source"))?;
    // SAFETY: Core Foundation's default mode is a valid static run loop mode.
    let mode = unsafe { kCFRunLoopDefaultMode };
    let run_loop = CFRunLoop::get_current();
    run_loop.add_source(&source, mode);
    // SAFETY: The port is alive for the full dispatch loop.
    unsafe { CGEventTapEnable(raw, true) };

    let result: io::Result<()> = (|| {
        initialized.send(Ok(())).map_err(io::Error::other)?;
        let mut secured = false;
        let mut last_permission_check = Instant::now();
        while !stop.load(Relaxed) {
            if last_permission_check.elapsed() >= Duration::from_millis(250) {
                if !MacInputSink::has_permission() {
                    return Err(tap_permission());
                }
                last_permission_check = Instant::now();
            }
            // SAFETY: raw remains owned by tap until after source removal.
            if !unsafe { CGEventTapIsEnabled(raw) } {
                return Err(io::Error::other("Quartz event tap was disabled"));
            }
            let secure = secure_input();
            if secure && !secured {
                state.borrow_mut().interrupt();
            }
            secured = secure;
            if !secure {
                let mut state = state.borrow_mut();
                let command = *state.commands.borrow();
                state.update(command);
            }
            CFRunLoop::run_in_mode(mode, POLL, false);
        }
        Ok(())
    })();
    state.borrow_mut().local();
    run_loop.remove_source(&source, mode);
    // SAFETY: Stop deliveries before dropping the port and callback state.
    unsafe { CFMachPortInvalidate(raw) };
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
        state.interrupt();
        return raw;
    }
    if raw.is_null() {
        return raw;
    }
    // SAFETY: A non-null event is live for the callback; Quartz retains ownership.
    let event = ManuallyDrop::new(unsafe { CGEvent::from_ptr(raw) });
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == EVENT_TAG
        || event.get_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID) != 0
    {
        return raw;
    }
    if state.capture(&event) {
        null_mut()
    } else {
        raw
    }
}

impl TapState {
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
            Command::Remote { .. } => {}
        }
    }

    fn interrupt(&mut self) {
        activity(&self.shortcuts);
        self.emergency();
    }

    fn emergency(&mut self) {
        self.local();
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
        let was_sending = self.cursor.is_some();
        self.local();
        if was_sending && let (Some(height), Some(edge)) = (height, &self.edge) {
            let _ = edge.layout.place(height);
        }
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
        if let Command::Local { return_height } = command {
            self.return_locally(return_height);
        }
        let kind = event.get_type();
        match kind {
            KeyDown | KeyUp | FlagsChanged => {
                activity(&self.shortcuts);
                let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                self.key(code, kind, event.get_flags())
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
                const MOMENTUM_PHASE: u32 = 123;
                if event.get_integer_value_field(MOMENTUM_PHASE) == 0 {
                    activity(&self.shortcuts);
                }
                self.cursor.is_some() && self.scroll(event)
            }
            _ => false,
        }
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

pub(super) fn secure_input() -> bool {
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

    struct RestorePointer(CGPoint);

    impl Drop for RestorePointer {
        fn drop(&mut self) {
            let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
            let _ = CGDisplay::warp_mouse_cursor_position(self.0);
        }
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
