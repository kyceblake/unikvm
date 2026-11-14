//! Clipboard revision tracking for one authenticated peer session.

use std::io;
use std::num::NonZeroU64;

use unikvm_core::device::DeviceId;
use unikvm_protocol::clipboard::{Revision, TextUpdate};

/// Keep one instance for the lifetime of a mutually enabled clipboard session.
/// A fresh session observes new clipboard changes without sending existing text.
pub struct Revisions {
    local: DeviceId,
    peer: DeviceId,
    latest: Option<Revision>,
}

impl Revisions {
    pub fn new(local: DeviceId, peer: DeviceId) -> Self {
        Self {
            local,
            peer,
            latest: None,
        }
    }

    /// Record a physical or application clipboard change. Native notifications
    /// caused by applying a peer update are filtered before calling this.
    pub fn local_change(&mut self, text: String) -> io::Result<TextUpdate> {
        let counter = self.latest.map_or(0, |revision| revision.counter.get());
        let counter = counter
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or_else(|| io::Error::other("clipboard revision exhausted"))?;
        let revision = Revision {
            counter,
            origin: self.local,
        };
        let update = TextUpdate::new(revision, text)?;
        self.latest = Some(revision);
        Ok(update)
    }

    /// Record a newer update after a successful native write.
    pub fn accept_remote(&mut self, update: &TextUpdate) -> bool {
        if !self.is_new_remote(update) {
            return false;
        }
        self.latest = Some(update.revision());
        true
    }

    /// Check before writing so a busy native clipboard can be retried.
    pub fn is_new_remote(&self, update: &TextUpdate) -> bool {
        let revision = update.revision();
        revision.origin == self.peer && self.latest.is_none_or(|latest| revision > latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: DeviceId = DeviceId([1; 16]);
    const B: DeviceId = DeviceId([2; 16]);

    #[test]
    fn concurrent_updates_converge_and_replay_cannot_replace_newer_text() {
        let mut a = Revisions::new(A, B);
        let mut b = Revisions::new(B, A);
        let from_a = a.local_change("first".into()).unwrap();
        let from_b = b.local_change("second".into()).unwrap();
        assert!(a.accept_remote(&from_b));
        assert!(!b.accept_remote(&from_a));
        assert!(!a.accept_remote(&from_b));

        let follow_up = a.local_change("third".into()).unwrap();
        assert!(follow_up.revision() > from_b.revision());
        assert!(b.accept_remote(&follow_up));
        assert!(!b.accept_remote(&from_a));
        assert_eq!(a.latest, b.latest);
    }

    #[test]
    fn rejects_updates_with_an_origin_other_than_the_pinned_peer() {
        let mut a = Revisions::new(A, B);
        let local = a.local_change("original".into()).unwrap();
        let mut unrelated = Revisions::new(DeviceId([3; 16]), A);
        let forged = unrelated.local_change("unrelated".into()).unwrap();
        assert!(!a.accept_remote(&local));
        assert!(!a.accept_remote(&forged));
        assert_eq!(a.latest, Some(local.revision()));
    }
}
