use std::io;
use std::thread::sleep;
use std::time::{Duration, Instant};

use unikvm_core::input::{ButtonState, InputEvent, KeyUsage};
use unikvm_platform::input::{Capture, MacInputSink, request_monitor_permission};

const USAGE: &str = "usage: input_probe [--request-permission | --exercise | --request-capture-permission | --capture]";

pub(super) fn run() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if args.next().is_some() {
        return Err(io::Error::other(USAGE));
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
        Some("--request-capture-permission") => {
            let _ = request_monitor_permission();
            Capture::start(true)?;
            println!("macOS capture tap installed.");
            Ok(())
        }
        Some("--capture") => capture(),
        _ => Err(io::Error::other(USAGE)),
    }
}

fn capture() -> io::Result<()> {
    let mut capture = Capture::start(true)?;
    let before = *capture.shortcuts.borrow();
    println!(
        "Focus a blank editor, press Ctrl+Shift+F9, release the chord, then type, move, click and scroll. Press Ctrl+Shift+F10 to end. You have 20 seconds."
    );
    let end = Instant::now() + Duration::from_secs(20);
    let (mut keys, mut moves, mut buttons, mut wheels) = (0, 0, 0, 0);
    while Instant::now() < end {
        let signal = *capture.shortcuts.borrow();
        if signal.stop != before.stop {
            capture.local();
            println!(
                "Captured key events {keys}, moves {moves}, buttons {buttons}, scrolls {wheels}."
            );
            return if keys > 0 && moves > 0 && buttons > 0 && wheels > 0 {
                Ok(())
            } else {
                Err(io::Error::other("missing one or more physical input types"))
            };
        }
        if signal.start != before.start {
            capture.renew(signal.start, Instant::now() + Duration::from_secs(1));
        }
        while let Ok(event) = capture.events.try_recv() {
            match event {
                InputEvent::Key { .. } => keys += 1,
                InputEvent::PointerMove { .. } => moves += 1,
                InputEvent::PointerButton { .. } => buttons += 1,
                InputEvent::Wheel { .. } => wheels += 1,
            }
        }
        sleep(Duration::from_millis(10));
    }
    Err(io::Error::other(
        "timed out waiting for the capture and return shortcuts",
    ))
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
