use std::io;
use std::thread::sleep;
use std::time::Duration;

use unikvm_core::input::{ButtonState, InputEvent, KeyUsage};
use unikvm_platform::input::MacInputSink;

pub(super) fn run() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if args.next().is_some() {
        return Err(io::Error::other(
            "usage: input_probe [--request-permission | --exercise]",
        ));
    }
    match command.as_deref() {
        None => {
            MacInputSink::new()?;
            println!("macOS input posting permission and desktop access passed.");
            Ok(())
        }
        Some("--request-permission") => {
            let _ = MacInputSink::request_permission();
            MacInputSink::new()?;
            println!("macOS input posting permission and desktop access passed.");
            Ok(())
        }
        Some("--exercise") => exercise(),
        _ => Err(io::Error::other(
            "usage: input_probe [--request-permission | --exercise]",
        )),
    }
}

fn exercise() -> io::Result<()> {
    // Check before the countdown so a permission denial cannot surprise the user with a prompt.
    MacInputSink::new()?;
    println!(
        "Focus a blank text editor in the next five seconds. The probe will post two keys, move the pointer, and scroll."
    );
    sleep(Duration::from_secs(5));
    let mut sink = MacInputSink::new()?;

    // Physical A and B positions. Drop the held Shift explicitly between them.
    sink.send(key(0xe1, ButtonState::Pressed))?;
    sink.send(key(0x04, ButtonState::Pressed))?;
    sink.send(key(0x04, ButtonState::Released))?;
    sink.release_all()?;
    sink.send(key(0x05, ButtonState::Pressed))?;
    sink.send(key(0x05, ButtonState::Released))?;
    sink.send(InputEvent::PointerMove { dx: 24, dy: 12 })?;
    sink.send(InputEvent::Wheel {
        horizontal: 0,
        vertical: -120,
    })?;
    println!("Posted the test events. On a US layout, verify the editor shows Ab.");
    Ok(())
}

fn key(usage: u16, state: ButtonState) -> InputEvent {
    InputEvent::Key {
        usage: KeyUsage(usage),
        state,
    }
}
