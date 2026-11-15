#![allow(unsafe_code)]

use std::cell::Cell;
use std::io;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont,
    NSProgressIndicator, NSProgressIndicatorStyle, NSTextField, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
};

use super::{HEADINGS, Row, TITLE, View};

const WIDTH: f64 = 420.0;
const HEIGHT: f64 = 210.0;
const MARGIN: f64 = 20.0;

pub(super) fn show(read: impl Fn() -> View + 'static) -> io::Result<()> {
    let main = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("AppKit requires the main thread"))?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        if !app.setActivationPolicy(NSApplicationActivationPolicy::Regular) {
            return Err(io::Error::other("could not activate the clipboard monitor"));
        }
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT));
        // SAFETY: The main thread owns the window. Rust retains it until the
        // event loop returns, so AppKit must not release it on close.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(main),
                bounds,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str(TITLE));
        let content = NSView::initWithFrame(NSView::alloc(main), bounds);
        let view = read();
        let connection = label(&content, main, view.connection, 16.0, false);
        let rows = std::array::from_fn(|index| {
            let top = 55.0 + index as f64 * 68.0;
            label(&content, main, HEADINGS[index], top, true);
            let text = label(&content, main, view.rows[index].text, top + 22.0, false);
            let progress = NSProgressIndicator::initWithFrame(
                NSProgressIndicator::alloc(main),
                rect(top + 44.0, 12.0),
            );
            progress.setStyle(NSProgressIndicatorStyle::Bar);
            progress.setIndeterminate(true);
            progress.setDisplayedWhenStopped(false);
            content.addSubview(&progress);
            let row = RowControls { text, progress };
            row.set_progress(view.rows[index].copying);
            row
        });
        window.setContentView(Some(&content));
        window.center();

        let delegate = Delegate::alloc(main).set_ivars(Monitor {
            read: Box::new(read),
            previous: Cell::new(view),
            connection,
            rows,
        });
        // SAFETY: NSObject initializes the allocated subclass with installed ivars.
        let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // SAFETY: refresh: is implemented on the retained target and takes one NSTimer.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.075,
                &delegate,
                sel!(refresh:),
                None,
                true,
            )
        };

        app.finishLaunching();
        window.makeKeyAndOrderFront(None);
        // Keep activation compatible with macOS releases before NSApplication::activate.
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        app.run();
        timer.invalidate();
        window.setDelegate(None);
        Ok(())
    })
}

fn label(
    parent: &NSView,
    main: MainThreadMarker,
    text: &str,
    top: f64,
    bold: bool,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), main);
    label.setFrame(rect(top, 20.0));
    if bold {
        label.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
    } else {
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    parent.addSubview(&label);
    label
}

fn rect(top: f64, height: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(MARGIN, HEIGHT - top - height),
        NSSize::new(WIDTH - 2.0 * MARGIN, height),
    )
}

struct RowControls {
    text: Retained<NSTextField>,
    progress: Retained<NSProgressIndicator>,
}

impl RowControls {
    fn update(&self, previous: Row, next: Row) {
        if previous.text != next.text {
            self.text.setStringValue(&NSString::from_str(next.text));
        }
        if previous.copying != next.copying {
            self.set_progress(next.copying);
        }
    }

    fn set_progress(&self, copying: bool) {
        // SAFETY: This main-thread indicator belongs to the content view.
        // No sender object is required for programmatic animation.
        unsafe {
            if copying {
                self.progress.startAnimation(None);
            } else {
                self.progress.stopAnimation(None);
            }
        }
    }
}

struct Monitor {
    read: Box<dyn Fn() -> View>,
    previous: Cell<View>,
    connection: Retained<NSTextField>,
    rows: [RowControls; 2],
}

define_class!(
    // SAFETY: NSObject permits subclassing. The ivars are main-thread only
    // and AppKit dispatches the window and run-loop callbacks on that thread.
    #[unsafe(super(NSObject))]
    #[name = "UniKVMClipboardMonitorDelegate"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Monitor]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWindowDelegate for Delegate {
        #[allow(non_snake_case)]
        #[unsafe(method(windowWillClose:))]
        fn windowWillClose(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).stop(None);
        }
    }

    impl Delegate {
        #[unsafe(method(refresh:))]
        fn refresh(&self, _timer: &NSTimer) {
            let view = self.ivars();
            let next = (view.read)();
            let previous = view.previous.replace(next);
            if previous.connection != next.connection {
                view.connection.setStringValue(&NSString::from_str(next.connection));
            }
            for (index, row) in view.rows.iter().enumerate() {
                row.update(previous.rows[index], next.rows[index]);
            }
        }
    }
);
