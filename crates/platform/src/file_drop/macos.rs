#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, OsStr};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use block2::DynBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSDragOperation,
    NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent,
    NSEventModifierFlags, NSEventType, NSFilePromiseProvider, NSFilePromiseProviderDelegate,
    NSFloatingWindowLevel, NSImage, NSImageNameMultipleDocuments, NSScreen, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSError, NSObject, NSObjectProtocol, NSOperationQueue, NSPoint, NSRect, NSSize,
    NSString, NSTimer, NSURL,
};
use unikvm_core::geometry::Edge;

use crate::file_drag_edge::EdgeHighlight;

const START_TIMEOUT: Duration = Duration::from_secs(45);
const PROMISE_TIMEOUT: Duration = Duration::from_secs(10);

/// Start a copy-only file promise at the display edge. Resolve its staged path
/// only when AppKit requests the file for the drop destination.
pub fn run_received(
    edge: Edge,
    name: &str,
    size: u64,
    verified_path: impl Fn() -> io::Result<PathBuf> + Send + Sync + 'static,
    ready: impl FnOnce() -> io::Result<()> + 'static,
) -> io::Result<bool> {
    let main = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("AppKit requires the main thread"))?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
            return Err(io::Error::other("could not start the Finder drop helper"));
        }
        let screen = NSScreen::screens(main)
            .firstObject()
            .ok_or_else(|| io::Error::other("no interactive display"))?;
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
        // SAFETY: The main thread owns and retains the window throughout this loop.
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
        // The outgoing edge observer stays active under the incoming drag strip.
        window.setLevel(NSFloatingWindowLevel + 1);
        let _highlight = EdgeHighlight::attach(&window);

        let promise = Promise::alloc().set_ivars(PromiseData {
            name: NSString::from_str(name),
            size,
            verified_path: Box::new(verified_path),
            state: Mutex::new(Delivery::Pending),
            queue: NSOperationQueue::new(),
        });
        // SAFETY: NSObject initializes the allocated delegate with its Rust ivars.
        let promise: Retained<Promise> = unsafe { msg_send![super(promise), init] };
        let provider = NSFilePromiseProvider::initWithFileType_delegate(
            NSFilePromiseProvider::alloc(),
            &NSString::from_str("public.data"),
            ProtocolObject::from_ref(&*promise),
        );
        let view = DragView::alloc(main).set_ivars(DragData {
            promise,
            provider,
            started: Cell::new(false),
            ended: Cell::new(None),
            opened: Instant::now(),
            ready: RefCell::new(Some(Box::new(ready))),
        });
        // SAFETY: The allocated subclass is initialized with an NSView frame on this thread.
        let view: Retained<DragView> = unsafe {
            msg_send![super(view), initWithFrame: NSRect::new(NSPoint::ZERO, frame.size)]
        };
        window.setContentView(Some(&view));
        app.finishLaunching();
        window.orderFrontRegardless();
        // SAFETY: The retained main-thread view implements checkCompletion: with one NSTimer.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.1,
                &view,
                sel!(checkCompletion:),
                None,
                true,
            )
        };
        app.run();
        timer.invalidate();
        window.orderOut(None);
        Ok(*view.ivars().promise.ivars().state.lock().unwrap() == Delivery::Copied)
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Pending,
    Writing,
    Copied,
    Failed,
}

struct PromiseData {
    name: Retained<NSString>,
    size: u64,
    verified_path: Box<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
    state: Mutex<Delivery>,
    queue: Retained<NSOperationQueue>,
}

define_class!(
    // SAFETY: Immutable metadata and the synchronized delivery are safe on AppKit's operation queue.
    #[unsafe(super(NSObject))]
    #[name = "UniKVMReceivedFilePromise"]
    #[thread_kind = AnyThread]
    #[ivars = PromiseData]
    struct Promise;

    unsafe impl NSObjectProtocol for Promise {}

    unsafe impl NSFilePromiseProviderDelegate for Promise {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(
            &self,
            _provider: &NSFilePromiseProvider,
            _kind: &NSString,
        ) -> Retained<NSString> {
            self.ivars().name.clone()
        }

        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn operation_queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
            self.ivars().queue.clone()
        }

        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let data = self.ivars();
            let can_write = {
                let mut state = data.state.lock().unwrap();
                if *state == Delivery::Pending && url.isFileURL() {
                    *state = Delivery::Writing;
                    true
                } else {
                    false
                }
            };
            let copied = can_write
                && match (data.verified_path)() {
                    Ok(path) => match write_promise(&path, url, data.size) {
                        Ok(()) => true,
                        Err(error) => {
                            eprintln!(
                                "Finder promise destination failed: kind={:?} os={:?}",
                                error.kind(),
                                error.raw_os_error()
                            );
                            false
                        }
                    },
                    Err(error) => {
                        eprintln!(
                            "Finder promise transfer failed: kind={:?} os={:?}",
                            error.kind(),
                            error.raw_os_error()
                        );
                        false
                    }
                };
            if copied {
                completion.call((null_mut(),));
            } else {
                // SAFETY: A static domain and no user info form a valid NSError.
                let error = unsafe {
                    NSError::errorWithDomain_code_userInfo(
                        &NSString::from_str("UniKVMFileDrop"),
                        1,
                        None,
                    )
                };
                completion.call((Retained::as_ptr(&error).cast_mut(),));
            }
            if can_write {
                *data.state.lock().unwrap() = if copied {
                    Delivery::Copied
                } else {
                    Delivery::Failed
                };
            }
        }
    }
);

type Ready = Box<dyn FnOnce() -> io::Result<()>>;

struct DragData {
    promise: Retained<Promise>,
    provider: Retained<NSFilePromiseProvider>,
    started: Cell<bool>,
    ended: Cell<Option<Instant>>,
    opened: Instant,
    ready: RefCell<Option<Ready>>,
}

define_class!(
    // SAFETY: AppKit invokes the view, drag source, and timer on the main thread.
    #[unsafe(super(NSView))]
    #[name = "UniKVMReceivedFileDrag"]
    #[thread_kind = MainThreadOnly]
    #[ivars = DragData]
    struct DragView;

    unsafe impl NSObjectProtocol for DragView {}

    unsafe impl NSDraggingSource for DragView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operations(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn drag_ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
            let data = self.ivars();
            data.ended.set(Some(Instant::now()));
            if operation != NSDragOperation::Copy {
                let mut state = data.promise.ivars().state.lock().unwrap();
                if *state == Delivery::Pending {
                    *state = Delivery::Failed;
                }
            }
            if let Some(window) = self.window() {
                window.orderOut(None);
            }
        }
    }

    impl DragView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let data = self.ivars();
            if data.started.replace(true) {
                return;
            }
            let item = NSDraggingItem::initWithPasteboardWriter(
                NSDraggingItem::alloc(), ProtocolObject::from_ref(&*data.provider),
            );
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let frame = NSRect::new(NSPoint::new(point.x - 16.0, point.y - 16.0), NSSize::new(32.0, 32.0));
            // SAFETY: AppKit initializes the exported image name.
            unsafe {
                let image = NSImage::imageNamed(NSImageNameMultipleDocuments);
                item.setDraggingFrame_contents(frame, image.as_deref().map(AsRef::as_ref));
            }
            let session = self.beginDraggingSessionWithItems_event_source(
                &NSArray::from_slice(&[&*item]), event, ProtocolObject::from_ref(self),
            );
            session.setAnimatesToStartingPositionsOnCancelOrFail(false);
        }

        #[unsafe(method(checkCompletion:))]
        fn check_completion(&self, _timer: &NSTimer) {
            let data = self.ivars();
            if let Some(ready) = data.ready.borrow_mut().take() {
                // Arm only after AppKit's event loop has entered and the window
                // is displayed; the first injected down must target this view.
                if let Some(window) = self.window() {
                    window.displayIfNeeded();
                }
                NSApplication::sharedApplication(self.mtm()).updateWindows();
                if let Err(error) = ready() {
                    eprintln!("Finder drop readiness failed: kind={:?}", error.kind());
                    *data.promise.ivars().state.lock().unwrap() = Delivery::Failed;
                }
            }
            let ended = data.ended.get();
            let finished = {
                let mut state = data.promise.ivars().state.lock().unwrap();
                if *state == Delivery::Pending
                    && ((!data.started.get() && data.opened.elapsed() >= START_TIMEOUT)
                        || ended.is_some_and(|end| end.elapsed() >= PROMISE_TIMEOUT))
                {
                    *state = Delivery::Failed;
                }
                (!data.started.get() || ended.is_some())
                    && matches!(*state, Delivery::Copied | Delivery::Failed)
            };
            if finished {
                let app = NSApplication::sharedApplication(self.mtm());
                app.stop(None);
                // Wake the AppKit event loop after stop called from a timer.
                if let Some(wakeup) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                    NSEventType::ApplicationDefined, NSPoint::ZERO, NSEventModifierFlags::empty(),
                    0.0, 0, None, 0, 0, 0,
                ) {
                    app.postEvent_atStart(&wakeup, false);
                }
            }
        }
    }
);

fn write_promise(source: &Path, url: &NSURL, size: u64) -> io::Result<()> {
    // AppKit invokes this delegate inside its coordinated write. Starting a new
    // coordinator here waits on that same destination claim and deadlocks.
    if !url.isFileURL() {
        return Err(io::Error::other("invalid file promise destination"));
    }
    // SAFETY: The NSURL keeps its NUL-terminated filesystem representation alive.
    let path = unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) };
    copy_file(source, Path::new(OsStr::from_bytes(path.to_bytes())), size)
}

fn copy_file(source: &Path, destination: &Path, size: u64) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|_| destination.is_absolute())
        .ok_or_else(|| io::Error::other("invalid drop destination"))?;
    let mut source = File::open(source)?.take(size.saturating_add(1));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if io::copy(&mut source, temporary.as_file_mut())? != size {
        return Err(io::Error::other("staged file size changed"));
    }
    temporary.as_file_mut().flush()?;
    // Publish the complete copy without replacing any existing file.
    temporary
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires native macOS file coordination"]
    fn promise_writes_inside_the_callers_existing_coordination() {
        use block2::StackBlock;
        use objc2_foundation::{NSFileCoordinator, NSFileCoordinatorWritingOptions};
        use std::ptr::NonNull;
        autoreleasepool(|_| {
            let source = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(source.path(), b"coordinated promise").unwrap();
            let folder = tempfile::tempdir().unwrap();
            let destination = folder.path().join("promise.bin");
            let url = NSURL::fileURLWithPath(&NSString::from_str(destination.to_str().unwrap()));
            let result = Cell::new(None);
            let writer = StackBlock::new(|url: NonNull<NSURL>| {
                // SAFETY: The outer coordinator keeps this URL alive during its accessor.
                let url = unsafe { url.as_ref() };
                result.set(Some(write_promise(source.path(), url, 19)));
            });
            NSFileCoordinator::new().coordinateWritingItemAtURL_options_error_byAccessor(
                &url,
                NSFileCoordinatorWritingOptions::empty(),
                None,
                &writer,
            );
            result.into_inner().unwrap().unwrap();
            assert_eq!(std::fs::read(destination).unwrap(), b"coordinated promise");
        });
    }

    #[test]
    fn publishes_complete_bytes_without_clobbering_or_leaving_partial_files() {
        let source = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(source.path(), b"sample").unwrap();
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("sample.bin");
        assert!(copy_file(source.path(), &destination, 5).is_err());
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        copy_file(source.path(), &destination, 6).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"sample");
        assert_eq!(std::fs::read(source.path()).unwrap(), b"sample");
        std::fs::write(source.path(), b"new").unwrap();
        assert_eq!(
            copy_file(source.path(), &destination, 3)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"sample");
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    }
}
