use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Default)]
pub struct PasteRequests(Arc<Mutex<Option<Offer>>>);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Offer {
    token: u64,
    sequence: u64,
    expires: Instant,
    requested: bool,
}

impl PasteRequests {
    pub fn arm(&self, token: u64, sequence: u64, expires: Instant) {
        *self.0.lock().unwrap() = Some(Offer {
            token,
            sequence,
            expires,
            requested: false,
        });
    }

    pub fn clear(&self, token: u64) {
        let mut offer = self.0.lock().unwrap();
        if offer.is_some_and(|offer| offer.token == token) {
            *offer = None;
        }
    }

    pub fn clear_unrequested(&self, token: u64) -> bool {
        let mut offer = self.0.lock().unwrap();
        if let Some(current) = offer.filter(|offer| offer.token == token) {
            if current.requested {
                return false;
            }
            *offer = None;
        }
        true
    }

    pub fn requested(&self, token: u64) -> bool {
        self.0
            .lock()
            .unwrap()
            .is_some_and(|offer| offer.token == token && offer.requested)
    }

    /// Call only for a paste key down. The system sequence is read without holding the lock.
    pub(crate) fn intercept(&self, current_sequence: impl FnOnce() -> io::Result<u64>) -> bool {
        let Some(candidate) = self.0.try_lock().ok().and_then(|offer| *offer) else {
            return false;
        };
        if (!candidate.requested && candidate.expires <= Instant::now())
            || current_sequence().ok() != Some(candidate.sequence)
        {
            return false;
        }
        if let Ok(mut guard) = self.0.try_lock()
            && let Some(offer) = guard
                .as_mut()
                .filter(|offer| offer.token == candidate.token)
        {
            offer.requested = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn newer_copy_and_replaced_or_expired_offers_leave_paste_alone() {
        let requests = PasteRequests::default();
        let expires = Instant::now() + Duration::from_secs(5);
        requests.arm(1, 20, expires);
        assert!(!requests.intercept(|| Ok(21)));
        assert!(!requests.requested(1));

        assert!(!requests.intercept(|| {
            requests.arm(2, 20, expires);
            Ok(20)
        }));
        requests.clear(1);
        assert!(requests.intercept(|| Ok(20)));
        assert!(requests.requested(2));
        assert!(!requests.clear_unrequested(2));
        requests.clear(2);
        assert!(!requests.intercept(|| panic!("sequence read without an offer")));

        requests.arm(3, 20, Instant::now());
        assert!(!requests.intercept(|| Ok(20)));
        assert!(!requests.requested(3));
    }
}
