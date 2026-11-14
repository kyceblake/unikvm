//! Scoped cursor visibility for the headless macOS sender.
//!
//! Public Quartz cursor visibility APIs normally require a foreground app. This
//! adapter uses a private WindowServer property on this process's connection;
//! it does not change a system preference. Resolve the private entry points at
//! runtime and reject capture when they are unavailable or hiding is ineffective.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_void};

use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGDisplay;

type DefaultConnection = unsafe extern "C" fn() -> i32;
type SetConnectionProperty = unsafe extern "C" fn(i32, i32, CFStringRef, CFTypeRef) -> i32;

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGCursorIsVisible() -> u32;
}

pub(super) fn is_visible() -> bool {
    // SAFETY: This argument-free Quartz function queries global cursor visibility.
    unsafe { CGCursorIsVisible() != 0 }
}

pub(super) struct BackgroundCursor {
    connection: i32,
    set_property: SetConnectionProperty,
    key: CFString,
    display: CGDisplay,
    background_enabled: bool,
    hidden: bool,
}

impl BackgroundCursor {
    pub(super) fn hide() -> Result<Self, ()> {
        // Darwin's RTLD_DEFAULT searches the current process's loaded images.
        const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
        // SAFETY: Both symbol names are static NUL-terminated C strings. No
        // library handle is opened, so these process-image symbols stay loaded.
        let (default_connection, set_property) = unsafe {
            (
                dlsym(RTLD_DEFAULT, c"_CGSDefaultConnection".as_ptr()),
                dlsym(RTLD_DEFAULT, c"CGSSetConnectionProperty".as_ptr()),
            )
        };
        if default_connection.is_null() || set_property.is_null() {
            return Err(());
        }
        // SAFETY: The resolved macOS functions have these connection/property
        // signatures. Missing symbols above fail closed instead of link-failing.
        let default_connection =
            unsafe { std::mem::transmute::<*mut c_void, DefaultConnection>(default_connection) };
        // SAFETY: Same symbol-resolution and lifetime requirements as above.
        let set_property =
            unsafe { std::mem::transmute::<*mut c_void, SetConnectionProperty>(set_property) };
        // SAFETY: The resolved argument-free function returns our own connection.
        let connection = unsafe { default_connection() };
        if connection <= 0 {
            return Err(());
        }
        let mut cursor = Self {
            connection,
            set_property,
            key: CFString::new("SetsCursorInBackground"),
            display: CGDisplay::main(),
            background_enabled: false,
            hidden: false,
        };
        cursor.set_background(true)?;
        cursor.background_enabled = true;
        cursor.display.hide_cursor().map_err(|_| ())?;
        cursor.hidden = true;
        if is_visible() {
            // Drop balances the hide count and restores the connection property.
            return Err(());
        }
        Ok(cursor)
    }

    fn set_background(&self, enabled: bool) -> Result<(), ()> {
        let value = CFBoolean::from(enabled);
        // SAFETY: The property name and Boolean stay live during this call. Both
        // connection arguments select this process, never another application's.
        let error = unsafe {
            (self.set_property)(
                self.connection,
                self.connection,
                self.key.as_concrete_TypeRef(),
                value.as_CFTypeRef(),
            )
        };
        if error == 0 { Ok(()) } else { Err(()) }
    }
}

impl Drop for BackgroundCursor {
    fn drop(&mut self) {
        // Show while background visibility is still permitted. Each successful
        // hide receives exactly one show, including partial-constructor failures.
        if self.hidden {
            let _ = self.display.show_cursor();
        }
        if self.background_enabled {
            let _ = self.set_background(false);
        }
    }
}
