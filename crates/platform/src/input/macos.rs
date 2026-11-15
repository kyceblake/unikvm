#![allow(unsafe_code)]

use std::io;
use std::time::{Duration, Instant};

use super::{EdgeLayout, NativePostTiming};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField, KeyCode,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect};
use core_graphics::sys::CGEventSourceRef;
use foreign_types::ForeignType;
use unikvm_core::geometry::Point;
use unikvm_core::input::{ButtonState, InputEvent, KeyUsage, PointerButton};

pub(super) const EVENT_TAG: i64 = 0x55_4e_49_4b_56_4d;
const DETENT: i64 = 120;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, seconds: f64);
    fn CGEventSourceSetLocalEventsFilterDuringSuppressionState(
        source: CGEventSourceRef,
        filter: u32,
        state: u32,
    );
}

pub struct MacInputSink {
    allow_locked_receive: bool,
    source: CGEventSource,
    keys: [bool; 256],
    buttons: [bool; 5],
    caps_lock: bool,
    pointer: CGPoint,
    display: CGDisplay,
    bounds: CGRect,
    wheel_remainder: [i64; 2],
    last_click: [Option<(Instant, CGPoint, i64)>; 5],
    post_profile: Option<PostProfile>,
}

#[derive(Default)]
struct PostProfile {
    checked: Option<(Duration, Instant)>,
    completed: Option<NativePostTiming>,
}

impl MacInputSink {
    pub fn enter_edge(&mut self, edge: EdgeLayout, height: u16) -> io::Result<()> {
        self.check_access()?;
        let point = edge.place(height)?;
        self.sync_edge_position(point)
    }

    fn sync_edge_position(&mut self, point: Point) -> io::Result<()> {
        self.display = CGDisplay::main();
        self.bounds = self.display.bounds();
        self.pointer = CGPoint::new(f64::from(point.x), f64::from(point.y));
        // Publish the new position immediately through the same tagged path as motion.
        self.move_pointer(0, 0)
    }

    pub fn exit_edge(&mut self, edge: EdgeLayout, dx: i32, dy: i32) -> io::Result<Option<u16>> {
        if self.check_access()? {
            return Ok(None);
        }
        self.exit_checked(edge, dx, dy)
    }

    pub fn move_or_exit(
        &mut self,
        edge: Option<EdgeLayout>,
        dx: i32,
        dy: i32,
    ) -> io::Result<Option<u16>> {
        let locked = self.begin_input_post()?;
        if !locked
            && let Some(edge) = edge
            && let Some(height) = self.exit_checked(edge, dx, dy)?
        {
            return Ok(Some(height));
        }
        self.move_pointer(dx, dy)?;
        Ok(None)
    }

    fn exit_checked(&mut self, edge: EdgeLayout, dx: i32, dy: i32) -> io::Result<Option<u16>> {
        if self.keys.contains(&true) || self.buttons.contains(&true) {
            return Ok(None);
        }
        let height = edge.crossing(
            Point {
                x: self.pointer.x.floor() as i32,
                y: self.pointer.y.floor() as i32,
            },
            dx,
            dy,
        );
        if let Some(height) = height {
            let point = edge.place_exit(height)?;
            self.sync_edge_position(point)?;
        }
        Ok(height)
    }

    /// Only this sink's authenticated injected primary hold can return a drag.
    /// Quartz hardware queries can also include injected button state. Physical
    /// takeover is enforced by the capture monitor and receiver session instead.
    pub fn remote_drag_height(&self, edge: EdgeLayout) -> Option<u16> {
        if !self.buttons[0]
            || self.buttons[1..].contains(&true)
            || self.keys.contains(&true)
            || self.is_locked_receiver()
            || !self.check_permission()
        {
            return None;
        }
        let dx = match edge.side {
            unikvm_core::geometry::Edge::Left => -4,
            unikvm_core::geometry::Edge::Right => 4,
            _ => return None,
        };
        edge.crossing(super::macos_edge::position().ok()?, dx, 0)
    }

    pub fn cancel_remote_drag(&mut self) -> io::Result<()> {
        self.check_access()?;
        if self.is_locked_receiver() {
            return Err(io::Error::other("remote drag is no longer eligible"));
        }
        super::macos_capture::cancel_native_drag()
            .map_err(|()| io::Error::other("could not cancel Finder's remote drag"))?;
        self.release_all()
    }

    /// Queries the current process's permission without showing a prompt.
    pub fn has_permission() -> bool {
        // SAFETY: This queries the WindowServer's posting permission for the current process.
        unsafe { CGPreflightPostEventAccess() }
    }

    /// Call in response to an explicit user action in an interactive session.
    pub fn request_permission() -> bool {
        // SAFETY: This argument-free API requests permission for the current process.
        unsafe { CGRequestPostEventAccess() }
    }

    /// The current slice confines synthetic pointer movement to the display containing it.
    pub fn new() -> io::Result<Self> {
        Self::with_locked_receive(false)
    }

    pub fn with_locked_receive(allow_locked_receive: bool) -> io::Result<Self> {
        if !Self::has_permission() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "grant Accessibility to the process posting input in System Settings",
            ));
        }
        if super::macos_session::receive_locked(
            allow_locked_receive,
            super::macos_capture::secure_input(),
        )
        .is_none()
            || super::macos_capture::physical_input_held()
        {
            return Err(io::Error::other(
                "release local keys and mouse buttons and exit Secure Input before sharing input",
            ));
        }
        let source = quartz(CGEventSource::new(CGEventSourceStateID::Private))?;
        // Keep local keyboard, mouse and system events enabled even during a synthetic drag.
        const PERMIT_ALL: u32 = 0x7;
        const REMOTE_DRAG: u32 = 1;
        // SAFETY: The calls use this live source and documented Quartz values.
        unsafe {
            CGEventSourceSetLocalEventsSuppressionInterval(source.as_ptr(), 0.0);
            CGEventSourceSetLocalEventsFilterDuringSuppressionState(
                source.as_ptr(),
                PERMIT_ALL,
                REMOTE_DRAG,
            );
        }
        let pointer = quartz(CGEvent::new(source.clone()))?.location();
        let displays = CGDisplay::active_displays().map_err(|status| {
            io::Error::other(format!("could not list active displays ({status})"))
        })?;
        let display = displays
            .into_iter()
            .map(CGDisplay::new)
            .find(|display| {
                let bounds = display.bounds();
                (bounds.origin.x..bounds.origin.x + bounds.size.width).contains(&pointer.x)
                    && (bounds.origin.y..bounds.origin.y + bounds.size.height).contains(&pointer.y)
            })
            .unwrap_or_else(CGDisplay::main);
        let bounds = display.bounds();
        if bounds.size.width < 1.0 || bounds.size.height < 1.0 {
            return Err(io::Error::other("could not read pointer display bounds"));
        }

        Ok(Self {
            allow_locked_receive,
            source,
            keys: [false; 256],
            buttons: [false; 5],
            caps_lock: false,
            pointer: clamp(pointer, bounds),
            display,
            bounds,
            wheel_remainder: [0; 2],
            last_click: [None; 5],
            post_profile: None,
        })
    }

    pub(super) fn pointer_display(&self) -> super::macos_motion::PointerDisplay {
        super::macos_motion::PointerDisplay::new(self.display.id)
    }

    pub fn send(&mut self, event: InputEvent) -> io::Result<()> {
        self.begin_input_post()?;
        match event {
            InputEvent::Key { usage, state } => self.key(usage, state),
            InputEvent::PointerButton { button, state } => self.button(button, state),
            InputEvent::PointerMove { dx, dy } => self.move_pointer(dx, dy),
            InputEvent::Wheel {
                horizontal,
                vertical,
            } => self.scroll(horizontal, vertical),
        }
    }

    pub fn check_permission(&self) -> bool {
        self.input_access().is_some()
    }

    fn input_access(&self) -> Option<bool> {
        if !Self::has_permission() {
            return None;
        }
        super::macos_session::receive_locked(
            self.allow_locked_receive,
            super::macos_capture::secure_input(),
        )
    }

    fn check_access(&self) -> io::Result<bool> {
        self.input_access().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "macOS input reception is not permitted in this session",
            )
        })
    }

    pub fn set_post_diagnostics(&mut self, enabled: bool) {
        self.post_profile = enabled.then(PostProfile::default);
    }

    pub fn take_post_timing(&mut self) -> Option<NativePostTiming> {
        let profile = self.post_profile.as_mut()?;
        profile.checked = None;
        profile.completed.take()
    }

    fn begin_input_post(&mut self) -> io::Result<bool> {
        let started = self.post_profile.as_ref().map(|_| Instant::now());
        let locked = self.check_access()?;
        if let (Some(started), Some(profile)) = (started, &mut self.post_profile) {
            let checked = Instant::now();
            profile.checked = Some((checked.duration_since(started), checked));
            profile.completed = None;
        }
        Ok(locked)
    }

    pub fn is_locked_receiver(&self) -> bool {
        self.allow_locked_receive && super::macos_session::is_locked()
    }

    /// Try every pending release, including when a permission change interrupted a session.
    pub fn release_all(&mut self) -> io::Result<()> {
        let mut result = Ok(());
        for usage in 0..self.keys.len() {
            if self.keys[usage] {
                result = self
                    .key(KeyUsage(usage as u16), ButtonState::Released)
                    .and(result);
            }
        }
        for button in [
            PointerButton::Primary,
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Back,
            PointerButton::Forward,
        ] {
            if self.buttons[mouse_button(button).0] {
                result = self.button(button, ButtonState::Released).and(result);
            }
        }
        self.wheel_remainder = [0; 2];
        self.last_click = [None; 5];
        result
    }

    fn key(&mut self, usage: KeyUsage, state: ButtonState) -> io::Result<()> {
        let Some(code) = key_code(usage) else {
            return Ok(());
        };
        let index = usize::from(usage.0);
        let pressed = state == ButtonState::Pressed;
        let was_pressed = self.keys[index];
        if !pressed && !was_pressed {
            return Ok(());
        }

        let event = quartz(CGEvent::new_keyboard_event(
            self.source.clone(),
            code,
            pressed,
        ))?;
        self.keys[index] = pressed;
        if usage.0 == 0x39 && pressed && !was_pressed {
            self.caps_lock = !self.caps_lock;
        }
        if matches!(usage.0, 0x39 | 0xe0..=0xe7) {
            event.set_type(CGEventType::FlagsChanged);
        } else if was_pressed && pressed {
            event.set_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT, 1);
        }
        let extra = if matches!(usage.0, 0x54..=0x63) {
            CGEventFlags::CGEventFlagNumericPad
        } else {
            CGEventFlags::empty()
        };
        self.post(&event, extra);
        Ok(())
    }

    fn button(&mut self, button: PointerButton, state: ButtonState) -> io::Result<()> {
        let (index, quartz_button, down, up, _) = mouse_button(button);
        let pressed = state == ButtonState::Pressed;
        if self.buttons[index] == pressed {
            return Ok(());
        }
        let event = quartz(CGEvent::new_mouse_event(
            self.source.clone(),
            if pressed { down } else { up },
            self.pointer,
            quartz_button,
        ))?;
        let now = Instant::now();
        let click_count = if pressed {
            let count = self.last_click[index]
                .filter(|(time, point, _)| {
                    now.duration_since(*time) <= Duration::from_millis(500)
                        && (point.x - self.pointer.x).hypot(point.y - self.pointer.y) <= 4.0
                })
                .map_or(1, |(_, _, count)| count.saturating_add(1));
            self.last_click[index] = Some((now, self.pointer, count));
            count
        } else {
            self.last_click[index].map_or(1, |(_, _, count)| count)
        };
        self.buttons[index] = pressed;
        event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, index as i64);
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_count);
        self.post(&event, CGEventFlags::empty());
        Ok(())
    }

    fn move_pointer(&mut self, dx: i32, dy: i32) -> io::Result<()> {
        let previous = self.pointer;
        let target = clamp(
            CGPoint::new(previous.x + f64::from(dx), previous.y + f64::from(dy)),
            self.bounds,
        );
        let pressed = [
            PointerButton::Primary,
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Back,
            PointerButton::Forward,
        ]
        .into_iter()
        .map(mouse_button)
        .find(|(index, ..)| self.buttons[*index]);
        let (index, button, _, _, kind) = pressed.unwrap_or((
            0,
            CGMouseButton::Left,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::MouseMoved,
        ));
        let event = quartz(CGEvent::new_mouse_event(
            self.source.clone(),
            kind,
            target,
            button,
        ))?;
        event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, index as i64);
        event.set_integer_value_field(
            EventField::MOUSE_EVENT_DELTA_X,
            (target.x - previous.x).round() as i64,
        );
        event.set_integer_value_field(
            EventField::MOUSE_EVENT_DELTA_Y,
            (target.y - previous.y).round() as i64,
        );
        self.pointer = target;
        self.post(&event, CGEventFlags::CGEventFlagNonCoalesced);
        Ok(())
    }

    fn scroll(&mut self, horizontal: i32, vertical: i32) -> io::Result<()> {
        let horizontal = i64::from(horizontal) + self.wheel_remainder[0];
        let vertical = i64::from(vertical) + self.wheel_remainder[1];
        let lines = [horizontal, vertical].map(|axis| (axis / DETENT) as i32);
        if lines != [0, 0] {
            // CoreGraphics uses positive axis 2 for scrolling toward the left.
            let event = quartz(CGEvent::new_scroll_event(
                self.source.clone(),
                ScrollEventUnit::LINE,
                2,
                lines[1],
                -lines[0],
                0,
            ))?;
            self.post(&event, CGEventFlags::empty());
        }
        self.wheel_remainder = [horizontal % DETENT, vertical % DETENT];
        Ok(())
    }

    fn post(&mut self, event: &CGEvent, extra: CGEventFlags) {
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, EVENT_TAG);
        let mut flags = extra;
        for (left, right, flag) in [
            (0xe0, 0xe4, CGEventFlags::CGEventFlagControl),
            (0xe1, 0xe5, CGEventFlags::CGEventFlagShift),
            (0xe2, 0xe6, CGEventFlags::CGEventFlagAlternate),
            (0xe3, 0xe7, CGEventFlags::CGEventFlagCommand),
        ] {
            flags.set(flag, self.keys[left] || self.keys[right]);
        }
        flags.set(CGEventFlags::CGEventFlagAlphaShift, self.caps_lock);
        event.set_flags(flags);
        if let Some(profile) = &mut self.post_profile
            && let Some((access, checked)) = profile.checked.take()
        {
            let posting = Instant::now();
            event.post(CGEventTapLocation::HID);
            profile.completed = Some(NativePostTiming {
                access,
                prepare: posting.duration_since(checked),
                post: posting.elapsed(),
            });
        } else {
            event.post(CGEventTapLocation::HID);
        }
    }
}

impl Drop for MacInputSink {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

fn quartz<T>(result: Result<T, ()>) -> io::Result<T> {
    result.map_err(|()| io::Error::other("CoreGraphics could not create an input event or source"))
}

fn clamp(point: CGPoint, rect: CGRect) -> CGPoint {
    CGPoint::new(
        point
            .x
            .clamp(rect.origin.x, rect.origin.x + rect.size.width - 1.0),
        point
            .y
            .clamp(rect.origin.y, rect.origin.y + rect.size.height - 1.0),
    )
}

fn mouse_button(
    button: PointerButton,
) -> (usize, CGMouseButton, CGEventType, CGEventType, CGEventType) {
    use CGEventType::{
        LeftMouseDown, LeftMouseDragged, LeftMouseUp, OtherMouseDown, OtherMouseDragged,
        OtherMouseUp, RightMouseDown, RightMouseDragged, RightMouseUp,
    };
    use CGMouseButton::{Center, Left, Right};

    match button {
        PointerButton::Primary => (0, Left, LeftMouseDown, LeftMouseUp, LeftMouseDragged),
        PointerButton::Secondary => (1, Right, RightMouseDown, RightMouseUp, RightMouseDragged),
        PointerButton::Middle => (2, Center, OtherMouseDown, OtherMouseUp, OtherMouseDragged),
        PointerButton::Back => (3, Center, OtherMouseDown, OtherMouseUp, OtherMouseDragged),
        PointerButton::Forward => (4, Center, OtherMouseDown, OtherMouseUp, OtherMouseDragged),
    }
}

pub(super) fn key_code(usage: KeyUsage) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        KeyCode::ANSI_A,
        KeyCode::ANSI_B,
        KeyCode::ANSI_C,
        KeyCode::ANSI_D,
        KeyCode::ANSI_E,
        KeyCode::ANSI_F,
        KeyCode::ANSI_G,
        KeyCode::ANSI_H,
        KeyCode::ANSI_I,
        KeyCode::ANSI_J,
        KeyCode::ANSI_K,
        KeyCode::ANSI_L,
        KeyCode::ANSI_M,
        KeyCode::ANSI_N,
        KeyCode::ANSI_O,
        KeyCode::ANSI_P,
        KeyCode::ANSI_Q,
        KeyCode::ANSI_R,
        KeyCode::ANSI_S,
        KeyCode::ANSI_T,
        KeyCode::ANSI_U,
        KeyCode::ANSI_V,
        KeyCode::ANSI_W,
        KeyCode::ANSI_X,
        KeyCode::ANSI_Y,
        KeyCode::ANSI_Z,
    ];
    const DIGITS: [u16; 10] = [
        KeyCode::ANSI_1,
        KeyCode::ANSI_2,
        KeyCode::ANSI_3,
        KeyCode::ANSI_4,
        KeyCode::ANSI_5,
        KeyCode::ANSI_6,
        KeyCode::ANSI_7,
        KeyCode::ANSI_8,
        KeyCode::ANSI_9,
        KeyCode::ANSI_0,
    ];
    const FUNCTION: [u16; 12] = [
        KeyCode::F1,
        KeyCode::F2,
        KeyCode::F3,
        KeyCode::F4,
        KeyCode::F5,
        KeyCode::F6,
        KeyCode::F7,
        KeyCode::F8,
        KeyCode::F9,
        KeyCode::F10,
        KeyCode::F11,
        KeyCode::F12,
    ];
    const KEYPAD_DIGITS: [u16; 10] = [
        KeyCode::ANSI_KEYPAD_1,
        KeyCode::ANSI_KEYPAD_2,
        KeyCode::ANSI_KEYPAD_3,
        KeyCode::ANSI_KEYPAD_4,
        KeyCode::ANSI_KEYPAD_5,
        KeyCode::ANSI_KEYPAD_6,
        KeyCode::ANSI_KEYPAD_7,
        KeyCode::ANSI_KEYPAD_8,
        KeyCode::ANSI_KEYPAD_9,
        KeyCode::ANSI_KEYPAD_0,
    ];
    const MODIFIERS: [u16; 8] = [
        KeyCode::CONTROL,
        KeyCode::SHIFT,
        KeyCode::OPTION,
        KeyCode::COMMAND,
        KeyCode::RIGHT_CONTROL,
        KeyCode::RIGHT_SHIFT,
        KeyCode::RIGHT_OPTION,
        KeyCode::RIGHT_COMMAND,
    ];

    let code = match usage.0 {
        key @ 0x04..=0x1d => LETTERS[usize::from(key - 0x04)],
        key @ 0x1e..=0x27 => DIGITS[usize::from(key - 0x1e)],
        key @ 0x3a..=0x45 => FUNCTION[usize::from(key - 0x3a)],
        key @ 0x59..=0x62 => KEYPAD_DIGITS[usize::from(key - 0x59)],
        key @ 0xe0..=0xe7 => MODIFIERS[usize::from(key - 0xe0)],
        0x28 => KeyCode::RETURN,
        0x29 => KeyCode::ESCAPE,
        0x2a => KeyCode::DELETE,
        0x2b => KeyCode::TAB,
        0x2c => KeyCode::SPACE,
        0x2d => KeyCode::ANSI_MINUS,
        0x2e => KeyCode::ANSI_EQUAL,
        0x2f => KeyCode::ANSI_LEFT_BRACKET,
        0x30 => KeyCode::ANSI_RIGHT_BRACKET,
        0x31 | 0x32 => KeyCode::ANSI_BACKSLASH,
        0x33 => KeyCode::ANSI_SEMICOLON,
        0x34 => KeyCode::ANSI_QUOTE,
        0x35 => KeyCode::ANSI_GRAVE,
        0x36 => KeyCode::ANSI_COMMA,
        0x37 => KeyCode::ANSI_PERIOD,
        0x38 => KeyCode::ANSI_SLASH,
        0x39 => KeyCode::CAPS_LOCK,
        0x49 => KeyCode::HELP,
        0x4a => KeyCode::HOME,
        0x4b => KeyCode::PAGE_UP,
        0x4c => KeyCode::FORWARD_DELETE,
        0x4d => KeyCode::END,
        0x4e => KeyCode::PAGE_DOWN,
        0x4f => KeyCode::RIGHT_ARROW,
        0x50 => KeyCode::LEFT_ARROW,
        0x51 => KeyCode::DOWN_ARROW,
        0x52 => KeyCode::UP_ARROW,
        0x53 => KeyCode::ANSI_KEYPAD_CLEAR,
        0x54 => KeyCode::ANSI_KEYPAD_DIVIDE,
        0x55 => KeyCode::ANSI_KEYPAD_MULTIPLY,
        0x56 => KeyCode::ANSI_KEYPAD_MINUS,
        0x57 => KeyCode::ANSI_KEYPAD_PLUS,
        0x58 => KeyCode::ANSI_KEYPAD_ENTER,
        0x63 => KeyCode::ANSI_KEYPAD_DECIMAL,
        0x64 => KeyCode::ISO_SECTION,
        0x67 => KeyCode::ANSI_KEYPAD_EQUAL,
        _ => return None,
    };
    Some(code)
}
