#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Back,
    Forward,
}

/// USB HID Keyboard/Keypad page usage, including individual left and right modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyUsage(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key {
        usage: KeyUsage,
        state: ButtonState,
    },
    PointerMove {
        dx: i32,
        dy: i32,
    },
    PointerButton {
        button: PointerButton,
        state: ButtonState,
    },
    /// Signed scroll deltas in units of 1/120 of a detent. Positive is right/up.
    Wheel {
        horizontal: i32,
        vertical: i32,
    },
}
