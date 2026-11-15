#![allow(unsafe_code)]

use std::{io, path::PathBuf};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSDragOperation,
    NSDraggingInfo, NSFloatingWindowLevel, NSPasteboard, NSPasteboardTypeFileURL, NSScreen,
    NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSTimer};
use unikvm_core::geometry::Edge;

pub fn run(edge: Edge) -> io::Result<()> {
    let main = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("AppKit requires the main thread"))?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
            return Err(io::Error::other("could not start the drag probe"));
        }
        let screen =
            NSScreen::mainScreen(main).ok_or_else(|| io::Error::other("no interactive display"))?;
        let bounds = screen.frame();
        let width = 18.0;
        let left = match edge {
            Edge::Left => bounds.origin.x,
            Edge::Right => bounds.origin.x + bounds.size.width - width,
            _ => return Err(io::Error::other("choose a horizontal edge")),
        };
        let frame = NSRect::new(
            NSPoint::new(left, bounds.origin.y),
            NSSize::new(width, bounds.size.height),
        );
        // SAFETY: The main thread owns and retains the window for this run loop.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(main),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        let delegate = Delegate::alloc(main).set_ivars(());
        // SAFETY: NSObject initializes the allocated main-thread subclass.
        let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        window.setLevel(NSFloatingWindowLevel);
        window.setBackgroundColor(Some(&NSColor::systemBlueColor()));
        // SAFETY: AppKit initializes the exported pasteboard type.
        let kind = unsafe { NSPasteboardTypeFileURL };
        window.registerForDraggedTypes(&NSArray::from_slice(&[kind]));
        // SAFETY: The retained delegate implements expire: with one NSTimer.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                45.0,
                &delegate,
                sel!(expire:),
                None,
                false,
            )
        };
        println!(
            "Drag a Finder file to the highlighted edge and hold it. The probe closes after 45 seconds."
        );
        app.finishLaunching();
        window.orderFrontRegardless();
        app.run();
        timer.invalidate();
        window.unregisterDraggedTypes();
        window.setDelegate(None);
        Ok(())
    })
}

fn references(board: &NSPasteboard) -> Option<Vec<PathBuf>> {
    // SAFETY: AppKit initializes the exported pasteboard type.
    let kind = unsafe { NSPasteboardTypeFileURL };
    let items = board.pasteboardItems()?;
    crate::clipboard::file_paths(&items, kind)
}

define_class!(
    // SAFETY: AppKit invokes these window delegate selectors on the main thread.
    #[unsafe(super(NSObject))]
    #[name = "UniKVMFileDragProbeDelegate"]
    #[thread_kind = MainThreadOnly]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSWindowDelegate for Delegate {}

    impl Delegate {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            super::report(references(&info.draggingPasteboard()));
            NSDragOperation::None
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            false
        }

        #[unsafe(method(expire:))]
        fn expire(&self, _timer: &NSTimer) {
            NSApplication::sharedApplication(self.mtm()).stop(None);
        }
    }
);
