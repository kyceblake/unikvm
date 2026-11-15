//! Observe a native file drag at the primary macOS display edge.

#![allow(unsafe_code)]

use std::{cell::Cell, io, path::PathBuf};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSDragOperation,
    NSDraggingInfo, NSFloatingWindowLevel, NSPasteboardTypeFileURL, NSScreen, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};
use unikvm_core::geometry::Edge;

pub fn observe(edge: Edge, file: impl Fn(PathBuf) + 'static) -> io::Result<()> {
    let main = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("AppKit requires the main thread"))?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
            return Err(io::Error::other("could not start the Finder drag helper"));
        }
        let screen = NSScreen::screens(main)
            .firstObject()
            .ok_or_else(|| io::Error::other("no interactive display"))?;
        let bounds = screen.frame();
        let width = 3.0;
        let x = match edge {
            Edge::Left => bounds.origin.x,
            Edge::Right => bounds.origin.x + bounds.size.width - width,
            _ => return Err(io::Error::other("invalid horizontal drag edge")),
        };
        let frame = NSRect::new(
            NSPoint::new(x, bounds.origin.y),
            NSSize::new(width, bounds.size.height),
        );
        // SAFETY: The AppKit main thread owns the window for this run loop.
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
        let delegate = Target::alloc(main).set_ivars(Callbacks {
            last: Cell::new(None),
            file: Box::new(file),
        });
        // SAFETY: NSObject initializes the allocated main-thread delegate.
        let delegate: Retained<Target> = unsafe { msg_send![super(delegate), init] };
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        window.setLevel(NSFloatingWindowLevel);
        window.setBackgroundColor(Some(&NSColor::systemBlueColor()));
        window.setAlphaValue(0.2);
        // SAFETY: AppKit provides this static pasteboard type.
        window.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
        app.finishLaunching();
        window.orderFrontRegardless();
        app.run();
        window.unregisterDraggedTypes();
        window.setDelegate(None);
        Ok(())
    })
}

struct Callbacks {
    last: Cell<Option<isize>>,
    file: Box<dyn Fn(PathBuf)>,
}

define_class!(
    // SAFETY: AppKit dispatches these delegates on its main thread.
    #[unsafe(super(NSObject))]
    #[name = "UniKVMNativeDragObserver"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Callbacks]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}
    unsafe impl NSWindowDelegate for Target {}

    impl Target {
        #[unsafe(method(draggingEntered:))]
        fn enter(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            let sequence = info.draggingSequenceNumber();
            if self.ivars().last.replace(Some(sequence)) != Some(sequence)
                && let Some(items) = info.draggingPasteboard().pasteboardItems()
                // SAFETY: The file URL pasteboard type is provided by AppKit.
                && let Some(paths) = crate::clipboard::file_paths(&items, unsafe { NSPasteboardTypeFileURL })
                && let [file] = paths.as_slice()
            {
                (self.ivars().file)(file.clone());
            }
            NSDragOperation::None
        }

        #[unsafe(method(performDragOperation:))]
        fn reject_drop(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool { false }
    }
);
