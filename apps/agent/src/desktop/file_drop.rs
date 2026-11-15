use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use unikvm_core::device::DeviceId;
use unikvm_protocol::local::file_drop::{Command, Offer, Reply};

#[derive(Clone, Default)]
pub(crate) struct NativeDrops(Arc<Mutex<Slots>>);

#[derive(Default)]
struct Slots {
    drop: Option<Drop>,
    expected: Option<Drag>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Drag {
    pub peer: DeviceId,
    pub connection: usize,
    pub nonce: NonZeroU64,
}

#[derive(Clone, Copy)]
struct Automatic {
    drag: Drag,
    armed: bool,
    released: bool,
}

struct Drop {
    token: u64,
    offer: Offer,
    expires: Instant,
    state: State,
    automatic: Option<Automatic>,
}

#[derive(PartialEq, Eq)]
enum State {
    Offered,
    Requested,
    Active,
    Ready(String),
    Finished(bool),
}

impl NativeDrops {
    pub fn offer(&self, token: u64, offer: Offer, expires: Instant) {
        self.0.lock().unwrap().drop = Some(Drop {
            token,
            offer,
            expires,
            state: State::Offered,
            automatic: None,
        });
    }

    pub fn reserve(&self, drag: Drag) -> bool {
        let mut slots = self.0.lock().unwrap();
        if slots.expected.is_some() {
            return false;
        }
        slots.expected = Some(drag);
        true
    }

    pub fn expects(&self, drag: Drag) -> bool {
        self.0.lock().unwrap().expected == Some(drag)
    }

    pub fn end_drag(&self, drag: Drag, delivered_release: bool) {
        let mut slots = self.0.lock().unwrap();
        if slots.expected == Some(drag) {
            slots.expected = None;
        }
        if !delivered_release
            && let Some(drop) = slots
                .drop
                .as_mut()
                .filter(|drop| drop.automatic.is_some_and(|auto| auto.drag == drag))
        {
            drop.state = State::Finished(false);
        }
    }

    pub fn offer_drag(&self, drag: Drag, token: u64, offer: Offer, expires: Instant) -> bool {
        let mut slots = self.0.lock().unwrap();
        if slots.expected != Some(drag) {
            return false;
        }
        slots.drop = Some(Drop {
            token,
            offer,
            expires,
            state: State::Offered,
            automatic: Some(Automatic {
                drag,
                armed: false,
                released: false,
            }),
        });
        true
    }

    pub fn offered(&self, drag: Drag) -> Option<u64> {
        let slots = self.0.lock().unwrap();
        let drop = slots.drop.as_ref()?;
        (drop.state == State::Offered && drop.automatic.is_some_and(|auto| auto.drag == drag))
            .then_some(drop.offer.id)
    }

    pub fn armed(&self, id: u64) -> bool {
        self.0.lock().unwrap().drop.as_ref().is_some_and(|drop| {
            drop.offer.id == id
                && drop.state == State::Offered
                && drop.automatic.is_some_and(|auto| auto.armed)
        })
    }

    pub fn release(&self, id: u64) -> bool {
        let mut slots = self.0.lock().unwrap();
        if let Some(auto) = slots
            .drop
            .as_mut()
            .filter(|drop| drop.offer.id == id && drop.state == State::Offered)
            .and_then(|drop| drop.automatic.as_mut())
            .filter(|auto| auto.armed)
        {
            auto.released = true;
            return true;
        }
        false
    }

    pub fn command(&self, command: Command) -> Reply {
        let mut slots = self.0.lock().unwrap();
        let Some(drop) = slots.drop.as_mut() else {
            return Reply::Unavailable;
        };
        match command {
            Command::Latest
                if drop.state == State::Offered
                    && drop.automatic.is_none()
                    && drop.expires > Instant::now() =>
            {
                Reply::Offer(drop.offer.clone())
            }
            Command::ForDrag(id)
                if drop.offer.id == id
                    && drop.state == State::Offered
                    && drop.automatic.is_some()
                    && drop.expires > Instant::now() =>
            {
                Reply::Offer(drop.offer.clone())
            }
            Command::Armed(id) if drop.offer.id == id && drop.state == State::Offered => {
                if let Some(auto) = drop.automatic.as_mut() {
                    auto.armed = true;
                    Reply::Recorded
                } else {
                    Reply::Unavailable
                }
            }
            Command::Begin(id)
                if drop.offer.id == id
                    && drop.state == State::Offered
                    && drop.automatic.is_none_or(|auto| auto.released)
                    && drop.expires > Instant::now() =>
            {
                drop.state = State::Requested;
                Reply::Pending
            }
            Command::Poll(id) if drop.offer.id == id => match &drop.state {
                State::Requested | State::Active => Reply::Pending,
                State::Ready(path) => Reply::Ready(path.clone()),
                _ => Reply::Unavailable,
            },
            Command::Finish { id, copied }
                if drop.offer.id == id
                    && (matches!(drop.state, State::Ready(_))
                        || (!copied
                            && (matches!(drop.state, State::Requested | State::Active)
                                || (drop.state == State::Offered
                                    && drop.automatic.is_some())))) =>
            {
                drop.state = State::Finished(copied);
                Reply::Recorded
            }
            _ => Reply::Unavailable,
        }
    }

    pub fn claim(&self, token: u64) -> bool {
        let mut slots = self.0.lock().unwrap();
        if let Some(drop) = slots
            .drop
            .as_mut()
            .filter(|drop| drop.token == token && drop.state == State::Requested)
        {
            drop.state = State::Active;
            true
        } else {
            false
        }
    }

    pub fn ready(&self, token: u64, path: &str) -> bool {
        let mut slots = self.0.lock().unwrap();
        if let Some(drop) = slots.drop.as_mut().filter(|drop| drop.token == token) {
            if drop.state == State::Active {
                drop.state = State::Ready(path.to_owned());
            }
            matches!(drop.state, State::Ready(_))
        } else {
            false
        }
    }

    pub fn finished(&self, token: u64) -> Option<bool> {
        let slots = self.0.lock().unwrap();
        match &slots
            .drop
            .as_ref()
            .filter(|drop| drop.token == token)?
            .state
        {
            State::Finished(copied) => Some(*copied),
            _ => None,
        }
    }

    pub fn clear(&self, token: u64) {
        let mut slots = self.0.lock().unwrap();
        if slots.drop.as_ref().is_some_and(|drop| drop.token == token) {
            slots.drop = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use unikvm_protocol::clipboard::FileOffer;

    #[test]
    fn only_the_claimed_drop_can_obtain_and_complete_verified_data() {
        let drops = NativeDrops::default();
        let offer = Offer {
            id: 5,
            file: FileOffer {
                name: "sample.txt".into(),
                size: 7,
            },
        };
        drops.offer(1, offer.clone(), Instant::now() + Duration::from_secs(10));
        assert_eq!(drops.command(Command::Latest), Reply::Offer(offer));
        assert_eq!(drops.command(Command::Begin(4)), Reply::Unavailable);
        assert_eq!(drops.command(Command::Poll(5)), Reply::Unavailable);
        assert_eq!(drops.command(Command::Begin(5)), Reply::Pending);
        assert_eq!(drops.command(Command::Begin(5)), Reply::Unavailable);
        assert!(!drops.claim(2));
        assert!(drops.claim(1));
        assert_eq!(
            drops.command(Command::Finish {
                id: 5,
                copied: true
            }),
            Reply::Unavailable
        );
        assert!(!drops.ready(2, "sample.txt"));
        assert!(drops.ready(1, "sample.txt"));
        assert_eq!(
            drops.command(Command::Poll(5)),
            Reply::Ready("sample.txt".into())
        );
        assert_eq!(
            drops.command(Command::Finish {
                id: 5,
                copied: true
            }),
            Reply::Recorded
        );
        assert_eq!(drops.finished(1), Some(true));
        drops.clear(1);
        assert_eq!(drops.command(Command::Poll(5)), Reply::Unavailable);
    }

    #[test]
    fn automatic_drop_requires_the_matching_session_and_a_forwarded_release() {
        let drops = NativeDrops::default();
        let route = Drag {
            peer: DeviceId([1; 16]),
            connection: 4,
            nonce: NonZeroU64::MIN,
        };
        let wrong = Drag {
            connection: 5,
            ..route
        };
        let offer = Offer {
            id: 60,
            file: FileOffer {
                name: "sample.txt".into(),
                size: 7,
            },
        };
        let expires = Instant::now() + Duration::from_secs(5);
        assert!(drops.reserve(route));
        assert!(!drops.reserve(wrong));
        assert!(!drops.offer_drag(wrong, 2, offer.clone(), expires));
        assert!(drops.offer_drag(route, 2, offer.clone(), expires));
        assert_eq!(drops.offered(route), Some(60));
        assert_eq!(drops.command(Command::Latest), Reply::Unavailable);
        assert_eq!(
            drops.command(Command::ForDrag(60)),
            Reply::Offer(offer.clone())
        );
        assert!(!drops.release(60));
        assert_eq!(drops.command(Command::Armed(60)), Reply::Recorded);
        assert_eq!(drops.command(Command::Begin(60)), Reply::Unavailable);
        drops.end_drag(route, false);
        assert_eq!(drops.finished(2), Some(false));

        assert!(drops.reserve(route));
        assert!(drops.offer_drag(route, 3, offer, expires));
        assert_eq!(drops.command(Command::Armed(60)), Reply::Recorded);
        assert!(drops.release(60));
        drops.end_drag(route, true);
        assert_eq!(drops.command(Command::Begin(60)), Reply::Pending);
        assert!(drops.claim(3));
    }
}
