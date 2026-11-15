//! Keep AppKit's edge hit target in place while fading its visible highlight.
#![allow(unsafe_code)]

use objc2::rc::Retained;
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSColor, NSWindow};
use objc2_foundation::{NSObject, NSRunLoop, NSRunLoopCommonModes, NSTimer};

// A fully transparent window can lose the first injected mouse-down.
const IDLE_ALPHA: f64 = 1.0 / 255.0;
const HOVER_ALPHA: f64 = 0.25;

pub(crate) struct EdgeHighlight(Retained<NSTimer>);

impl EdgeHighlight {
    pub(crate) fn attach(window: &Retained<NSWindow>) -> Self {
        window.setHasShadow(false);
        window.setBackgroundColor(Some(&NSColor::systemBlueColor()));
        window.setAlphaValue(IDLE_ALPHA);

        let target = Highlight::alloc(window.mtm()).set_ivars(window.clone());
        // SAFETY: NSObject initializes the main-thread target, which implements
        // updateHighlight: with one NSTimer. Schedule it on the same thread.
        let timer = unsafe {
            let target: Retained<Highlight> = msg_send![super(target), init];
            let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.04,
                &target,
                sel!(updateHighlight:),
                None,
                true,
            );
            NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
            timer
        };
        Self(timer)
    }
}

impl Drop for EdgeHighlight {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}

define_class!(
    // SAFETY: The window and its timer are owned by the AppKit main thread.
    #[unsafe(super(NSObject))]
    #[name = "UniKVMEdgeHighlight"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Retained<NSWindow>]
    struct Highlight;

    impl Highlight {
        #[unsafe(method(updateHighlight:))]
        fn update(&self, _timer: &NSTimer) {
            use objc2::DefinedClass;
            let window = self.ivars();
            if !window.isVisible() {
                return;
            }
            let point = window.mouseLocationOutsideOfEventStream();
            let size = window.frame().size;
            let hovered = (0.0..size.width).contains(&point.x)
                && (0.0..size.height).contains(&point.y)
                && crate::input::cursor_is_visible();
            let alpha = if hovered { HOVER_ALPHA } else { IDLE_ALPHA };
            if window.alphaValue() != alpha {
                window.setAlphaValue(alpha);
            }
        }
    }
);
