use std::sync::{Arc, Mutex};
use std::time::Instant;

use unikvm_protocol::local::file_drop::{Command, Offer, Reply};

#[derive(Clone, Default)]
pub(crate) struct NativeDrops(Arc<Mutex<Option<Drop>>>);

struct Drop {
    token: u64,
    offer: Offer,
    expires: Instant,
    state: State,
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
        *self.0.lock().unwrap() = Some(Drop {
            token,
            offer,
            expires,
            state: State::Offered,
        });
    }

    pub fn command(&self, command: Command) -> Reply {
        let mut slot = self.0.lock().unwrap();
        let Some(drop) = slot.as_mut() else {
            return Reply::Unavailable;
        };
        match command {
            Command::Latest if drop.state == State::Offered && drop.expires > Instant::now() => {
                Reply::Offer(drop.offer.clone())
            }
            Command::Begin(id)
                if drop.offer.id == id
                    && drop.state == State::Offered
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
                        || (!copied && matches!(drop.state, State::Requested | State::Active))) =>
            {
                drop.state = State::Finished(copied);
                Reply::Recorded
            }
            _ => Reply::Unavailable,
        }
    }

    pub fn claim(&self, token: u64) -> bool {
        let mut slot = self.0.lock().unwrap();
        if let Some(drop) = slot
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
        let mut slot = self.0.lock().unwrap();
        if let Some(drop) = slot.as_mut().filter(|drop| drop.token == token) {
            if drop.state == State::Active {
                drop.state = State::Ready(path.to_owned());
            }
            matches!(drop.state, State::Ready(_))
        } else {
            false
        }
    }

    pub fn finished(&self, token: u64) -> Option<bool> {
        let slot = self.0.lock().unwrap();
        match &slot.as_ref().filter(|drop| drop.token == token)?.state {
            State::Finished(copied) => Some(*copied),
            _ => None,
        }
    }

    pub fn clear(&self, token: u64) {
        let mut slot = self.0.lock().unwrap();
        if slot.as_ref().is_some_and(|drop| drop.token == token) {
            *slot = None;
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
}
