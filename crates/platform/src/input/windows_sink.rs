#![allow(unsafe_code)]

use std::io;

use super::EdgeLayout;
use unikvm_core::input::{ButtonState, InputEvent, KeyUsage, PointerButton};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};

use super::windows::{check_input_desktop, key_usage, physical_input_held};

pub(super) const EVENT_TAG: usize = 0x554b564d;
const BUTTONS: [PointerButton; 5] = [
    PointerButton::Primary,
    PointerButton::Secondary,
    PointerButton::Middle,
    PointerButton::Back,
    PointerButton::Forward,
];

pub struct WindowsInputSink {
    keys: [bool; 256],
    buttons: [bool; 5],
}

impl WindowsInputSink {
    pub fn enter_edge(&mut self, edge: EdgeLayout, height: u16) -> io::Result<()> {
        edge.place(height)?;
        Ok(())
    }

    pub fn exit_edge(&mut self, edge: EdgeLayout, dx: i32, dy: i32) -> io::Result<Option<u16>> {
        if self.keys.contains(&true) || self.buttons.contains(&true) {
            return Ok(None);
        }
        let height = edge.crossing(super::windows_edge::position()?, dx, dy);
        if let Some(height) = height {
            edge.place_exit(height)?;
        }
        Ok(height)
    }

    pub(super) fn pointer_display(&self) -> super::windows_motion::PointerDisplay {
        super::windows_motion::PointerDisplay::default()
    }

    pub fn has_permission() -> bool {
        check_input_desktop().is_ok()
    }

    pub fn new() -> io::Result<Self> {
        Self::with_locked_receive(false)
    }

    /// A user-session process cannot inject into Winlogon's secure desktop.
    /// Keep the opt-in explicit until an authenticated privileged helper exists.
    pub fn with_locked_receive(allow_locked_receive: bool) -> io::Result<Self> {
        if let Err(error) = check_input_desktop() {
            return Err(if allow_locked_receive {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "active Windows desktop unavailable; locked receiving requires a privileged helper not included in this build ({error})"
                    ),
                )
            } else {
                error
            });
        }
        if physical_input_held() {
            return Err(io::Error::other(
                "release local keys and mouse buttons before sharing input",
            ));
        }
        Ok(Self {
            keys: [false; 256],
            buttons: [false; 5],
        })
    }

    pub fn check_permission(&self) -> bool {
        Self::has_permission()
    }

    pub fn is_locked_receiver(&self) -> bool {
        false
    }

    pub fn send(&mut self, event: InputEvent) -> io::Result<()> {
        match event {
            InputEvent::Key { usage, state } => self.key(usage, state),
            InputEvent::PointerButton { button, state } => self.button(button, state),
            InputEvent::PointerMove { dx, dy } => inject(mouse_input(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE,
                0,
                dx,
                dy,
            )),
            InputEvent::Wheel {
                horizontal,
                vertical,
            } => {
                for (delta, flag) in [
                    (vertical, MOUSEEVENTF_WHEEL),
                    (horizontal, MOUSEEVENTF_HWHEEL),
                ] {
                    if delta != 0 {
                        inject(mouse_input(flag, delta as u32, 0, 0))?;
                    }
                }
                Ok(())
            }
        }
    }

    pub fn release_all(&mut self) -> io::Result<()> {
        let mut result = Ok(());
        // Release ordinary keys and buttons before modifiers.
        for (index, button) in BUTTONS.into_iter().enumerate() {
            if self.buttons[index] {
                result = self.button(button, ButtonState::Released).and(result);
            }
        }
        for index in 0..self.keys.len() {
            if self.keys[index] {
                result = self
                    .key(KeyUsage(index as u16), ButtonState::Released)
                    .and(result);
            }
        }
        result
    }

    fn key(&mut self, usage: KeyUsage, state: ButtonState) -> io::Result<()> {
        let Some((scan, extended)) = scan_code(usage) else {
            return Ok(());
        };
        let pressed = state == ButtonState::Pressed;
        let key = &mut self.keys[usize::from(usage.0)];
        if !pressed && !*key {
            return Ok(());
        }
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !pressed {
            flags |= KEYEVENTF_KEYUP;
        }
        inject(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wScan: scan,
                    dwFlags: flags,
                    dwExtraInfo: EVENT_TAG,
                    ..Default::default()
                },
            },
        })?;
        *key = pressed;
        Ok(())
    }

    fn button(&mut self, button: PointerButton, state: ButtonState) -> io::Result<()> {
        let (index, down, up, data) = match button {
            PointerButton::Primary => (0, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
            PointerButton::Secondary => (1, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
            PointerButton::Middle => (2, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
            PointerButton::Back => (3, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
            PointerButton::Forward => (4, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
        };
        let pressed = state == ButtonState::Pressed;
        if self.buttons[index] == pressed {
            return Ok(());
        }
        inject(mouse_input(if pressed { down } else { up }, data, 0, 0))?;
        self.buttons[index] = pressed;
        Ok(())
    }
}

impl Drop for WindowsInputSink {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

fn mouse_input(flags: u32, data: u32, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                dwExtraInfo: EVENT_TAG,
                ..Default::default()
            },
        },
    }
}

fn inject(input: INPUT) -> io::Result<()> {
    // SAFETY: The pointer refers to one initialized INPUT with the proper union member.
    if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } != 1 {
        return Err(io::Error::other(format!(
            "Windows SendInput rejected an event ({}); check the active desktop and app integrity level",
            io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn scan_code(usage: KeyUsage) -> Option<(u16, bool)> {
    // Use the same Set 1 mapping as capture, including the extended-key bit.
    [false, true].into_iter().find_map(|extended| {
        (1..=0x5c)
            .find(|&scan| key_usage(scan, extended) == Some(usage.0))
            .map(|scan| (scan as u16, extended))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_usages_to_the_same_scan_codes_as_capture() {
        assert_eq!(scan_code(KeyUsage(0x04)), Some((0x1e, false)));
        assert_eq!(scan_code(KeyUsage(0xe0)), Some((0x1d, false)));
        assert_eq!(scan_code(KeyUsage(0xe4)), Some((0x1d, true)));
        assert_eq!(scan_code(KeyUsage(0xe7)), Some((0x5c, true)));
        assert_eq!(scan_code(KeyUsage(0xff)), None);
    }

    #[test]
    #[ignore = "requires an interactive Windows input desktop"]
    fn sink_posts_and_releases_without_notifying_capture() {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::{
            Input::KeyboardAndMouse::GetAsyncKeyState,
            WindowsAndMessaging::{GetCursorPos, SetCursorPos},
        };

        let capture = super::super::Capture::start(false).unwrap();
        let baseline = *capture.shortcuts.borrow();
        let mut sink = WindowsInputSink::new().unwrap();
        let mut before = POINT::default();
        let mut after = POINT::default();
        // SAFETY: Each GetCursorPos call writes to a live POINT.
        assert_ne!(unsafe { GetCursorPos(&mut before) }, 0);
        sink.send(InputEvent::PointerMove { dx: 12, dy: 0 })
            .unwrap();
        assert_ne!(unsafe { GetCursorPos(&mut after) }, 0);
        if after.x == before.x {
            sink.send(InputEvent::PointerMove { dx: -12, dy: 0 })
                .unwrap();
            assert_ne!(unsafe { GetCursorPos(&mut after) }, 0);
        }
        assert_ne!(before.x, after.x);
        sink.send(InputEvent::Key {
            usage: KeyUsage(0xe5),
            state: ButtonState::Pressed,
        })
        .unwrap();
        // SAFETY: Query the right shift state for this interactive desktop.
        assert!(unsafe { GetAsyncKeyState(0xa1) } < 0);
        drop(sink);
        // SAFETY: Query the same key to verify cleanup.
        assert!(unsafe { GetAsyncKeyState(0xa1) } >= 0);
        assert_eq!(*capture.shortcuts.borrow(), baseline);
        drop(capture);
        // SAFETY: Restore the cursor position saved before the probe.
        unsafe { SetCursorPos(before.x, before.y) };
    }
}
