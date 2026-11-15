#![allow(unsafe_code)]

use std::{
    ffi::CString,
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::NonNull,
};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSDragOperation,
    NSDraggingContext, NSDraggingInfo, NSDraggingItem, NSDraggingSession, NSDraggingSource,
    NSEvent, NSFloatingWindowLevel, NSImage, NSImageNameMultipleDocuments, NSPasteboard,
    NSPasteboardTypeFileURL, NSScreen, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSTimer, NSURL,
};
use unikvm_core::geometry::Edge;

use super::Source;

pub fn run(edge: Edge, source: Option<Source<'_>>) -> io::Result<()> {
    let source = match source {
        Some(Source::File(path)) => Some(path),
        Some(Source::Deferred(path)) => return run_deferred(edge, path),
        None => None,
    };
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
        if let Some(path) = source {
            let path = CString::new(path.as_os_str().as_bytes())?;
            let representation =
                NonNull::new(path.as_ptr().cast_mut()).expect("CString has a non-null buffer");
            // SAFETY: The absolute filesystem path is NUL-terminated and read only during the call.
            let url = unsafe {
                NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
                    representation,
                    false,
                    None,
                )
            };
            let view = FileSource::alloc(main).set_ivars(url);
            // SAFETY: The allocated subclass is initialized with an NSView frame on this thread.
            let view: Retained<FileSource> = unsafe {
                msg_send![super(view), initWithFrame: NSRect::new(NSPoint::ZERO, frame.size)]
            };
            window.setContentView(Some(&view));
            println!("Press on the highlighted edge and drag into an empty Finder folder.");
        } else {
            // SAFETY: AppKit initializes the exported pasteboard type.
            let kind = unsafe { NSPasteboardTypeFileURL };
            window.registerForDraggedTypes(&NSArray::from_slice(&[kind]));
            println!(
                "Drag a Finder file to the highlighted edge and hold it. The probe closes after 45 seconds."
            );
        }
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
        app.finishLaunching();
        window.orderFrontRegardless();
        app.run();
        timer.invalidate();
        window.unregisterDraggedTypes();
        window.setDelegate(None);
        Ok(())
    })
}

fn run_deferred(edge: Edge, path: &Path) -> io::Result<()> {
    let path = path.canonicalize()?;
    let metadata = path.metadata()?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|_| metadata.is_file())
        .ok_or_else(|| io::Error::other("choose a regular file with a UTF-8 name"))?
        .to_owned();
    let copied = crate::file_drop::run_received(
        edge,
        &name,
        metadata.len(),
        move || {
            println!("Finder requested the promised file after release.");
            Ok(path.clone())
        },
        || {
            println!("Press on the highlighted edge and drag into an empty Finder folder.");
            Ok(())
        },
    )?;
    super::report_drop(copied);
    Ok(())
}

define_class!(
    // SAFETY: AppKit invokes this view and its drag source callbacks on the main thread.
    #[unsafe(super(NSView))]
    #[name = "UniKVMFileDragProbeSource"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Retained<NSURL>]
    struct FileSource;

    unsafe impl NSObjectProtocol for FileSource {}

    unsafe impl NSDraggingSource for FileSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operations(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn drag_ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
            super::report_drop(operation == NSDragOperation::Copy);
            NSApplication::sharedApplication(self.mtm()).stop(None);
        }
    }

    impl FileSource {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let item = NSDraggingItem::initWithPasteboardWriter(
                NSDraggingItem::alloc(), ProtocolObject::from_ref(&**self.ivars()),
            );
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let frame = NSRect::new(NSPoint::new(point.x - 16.0, point.y - 16.0), NSSize::new(32.0, 32.0));
            // SAFETY: AppKit initializes the name. NSImage is a supported dragging image.
            unsafe {
                let image = NSImage::imageNamed(NSImageNameMultipleDocuments);
                item.setDraggingFrame_contents(frame, image.as_deref().map(AsRef::as_ref));
            }
            let session = self.beginDraggingSessionWithItems_event_source(
                &NSArray::from_slice(&[&*item]), event, ProtocolObject::from_ref(self),
            );
            session.setAnimatesToStartingPositionsOnCancelOrFail(false);
        }
    }
);

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
