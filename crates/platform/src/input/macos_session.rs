//! Read lock state only; never inspect login-window fields or credentials.
#![allow(unsafe_code)]

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

#[derive(Default)]
struct Session {
    eligible: bool,
    locked: bool,
}

fn current() -> Session {
    // SAFETY: Quartz returns null or a retained session dictionary, adopted below.
    let raw = unsafe { CGSessionCopyCurrentDictionary() };
    if raw.is_null() {
        return Session::default();
    }
    // SAFETY: The copy API transfers one owned CFDictionary reference.
    let session: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(raw) };
    let boolean = |key| {
        session
            .find(CFString::new(key))
            .and_then(|value| value.downcast::<CFBoolean>())
            .map(bool::from)
    };
    let uid = session
        .find(CFString::new("kCGSSessionUserIDKey"))
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|value| value.to_i64());
    let locked = boolean("CGSSessionScreenIsLocked");
    let malformed_lock =
        session.contains_key(&CFString::new("CGSSessionScreenIsLocked")) && locked.is_none();
    Session {
        eligible: !malformed_lock
            && own_console(
                boolean("kCGSSessionOnConsoleKey"),
                boolean("kCGSessionLoginDoneKey"),
                uid,
                rustix::process::geteuid().as_raw(),
            ),
        locked: locked.unwrap_or(false),
    }
}

fn own_console(
    on_console: Option<bool>,
    login_done: Option<bool>,
    uid: Option<i64>,
    own_uid: u32,
) -> bool {
    on_console == Some(true) && login_done == Some(true) && uid == Some(i64::from(own_uid))
}

pub(super) fn capture_unavailable() -> bool {
    let session = current();
    !session.eligible || session.locked
}

pub(super) fn is_locked() -> bool {
    let session = current();
    session.eligible && session.locked
}

pub(super) fn receive_allowed(allow_locked: bool, secure: bool) -> bool {
    let session = current();
    session.eligible && may_receive(allow_locked, session.locked, secure)
}

pub(super) fn may_receive(allow_locked: bool, locked: bool, secure: bool) -> bool {
    if locked { allow_locked } else { !secure }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_logged_in_own_console_session_is_eligible() {
        assert!(own_console(Some(true), Some(true), Some(501), 501));
        assert!(!own_console(Some(false), Some(true), Some(501), 501));
        assert!(!own_console(Some(true), Some(false), Some(501), 501));
        assert!(!own_console(Some(true), Some(true), Some(502), 501));
        assert!(!own_console(None, Some(true), Some(501), 501));
        assert!(!own_console(Some(true), None, Some(501), 501));
        assert!(!own_console(Some(true), Some(true), None, 501));
    }

    #[test]
    fn locked_receive_is_opt_in_and_does_not_allow_other_secure_fields() {
        assert!(may_receive(false, false, false));
        assert!(!may_receive(false, true, false));
        assert!(!may_receive(false, true, true));
        assert!(may_receive(true, true, false));
        assert!(may_receive(true, true, true));
        assert!(!may_receive(true, false, true));
        assert!(!may_receive(false, false, true));
    }
}
