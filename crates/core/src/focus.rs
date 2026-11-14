use crate::device::DeviceId;

/// An opaque session number supplied by the future control protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlSession {
    pub peer: DeviceId,
    pub id: SessionId,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FocusState {
    #[default]
    Local,
    Requesting(ControlSession),
    Sending(ControlSession),
    Receiving(ControlSession),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    Request(ControlSession),
    Grant(ControlSession),
    AcceptIncoming(ControlSession),
    EndSession(ControlSession),
    PeerDisconnected(DeviceId),
    LocalActivity,
    ReturnToLocal,
}

impl FocusState {
    /// Apply an already authorized event. The caller validates peer identity, session freshness,
    /// and lease timing, and uses state changes to start or release platform input ownership.
    pub fn transition(self, event: FocusEvent) -> Self {
        match (self, event) {
            (Self::Local, FocusEvent::Request(session)) => Self::Requesting(session),
            (Self::Local, FocusEvent::AcceptIncoming(session)) => Self::Receiving(session),
            (Self::Requesting(active), FocusEvent::Grant(granted)) if active == granted => {
                Self::Sending(active)
            }
            (Self::Receiving(_), FocusEvent::LocalActivity) | (_, FocusEvent::ReturnToLocal) => {
                Self::Local
            }
            (
                Self::Requesting(active) | Self::Sending(active) | Self::Receiving(active),
                FocusEvent::EndSession(ended),
            ) if active == ended => Self::Local,
            (
                Self::Requesting(active) | Self::Sending(active) | Self::Receiving(active),
                FocusEvent::PeerDisconnected(peer),
            ) if active.peer == peer => Self::Local,
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlSession, FocusEvent, FocusState, SessionId};
    use crate::device::DeviceId;

    const FIRST: ControlSession = ControlSession {
        peer: DeviceId([1; 16]),
        id: SessionId(1),
    };
    const NEWER: ControlSession = ControlSession {
        peer: FIRST.peer,
        id: SessionId(2),
    };
    const OTHER: ControlSession = ControlSession {
        peer: DeviceId([2; 16]),
        id: FIRST.id,
    };

    #[test]
    fn sends_only_after_the_matching_grant() {
        let pending = FocusState::default().transition(FocusEvent::Request(FIRST));
        assert_eq!(pending, FocusState::Requesting(FIRST));
        for invalid in [NEWER, OTHER] {
            assert_eq!(pending.transition(FocusEvent::Grant(invalid)), pending);
        }
        let sending = pending.transition(FocusEvent::Grant(FIRST));
        assert_eq!(sending, FocusState::Sending(FIRST));
        assert_eq!(sending.transition(FocusEvent::LocalActivity), sending);
    }

    #[test]
    fn allows_only_one_receiver_owner_and_prioritizes_local_input() {
        let receiving = FocusState::Local.transition(FocusEvent::AcceptIncoming(FIRST));
        assert_eq!(receiving, FocusState::Receiving(FIRST));
        assert_eq!(
            receiving.transition(FocusEvent::AcceptIncoming(OTHER)),
            receiving
        );
        assert_eq!(receiving.transition(FocusEvent::Request(OTHER)), receiving);
        assert_eq!(
            receiving.transition(FocusEvent::LocalActivity),
            FocusState::Local
        );
    }

    #[test]
    fn clears_only_the_active_session_on_termination_or_disconnect() {
        for state in [
            FocusState::Requesting(FIRST),
            FocusState::Sending(FIRST),
            FocusState::Receiving(FIRST),
        ] {
            assert_eq!(state.transition(FocusEvent::EndSession(NEWER)), state);
            assert_eq!(
                state.transition(FocusEvent::PeerDisconnected(OTHER.peer)),
                state
            );
            for event in [
                FocusEvent::EndSession(FIRST),
                FocusEvent::PeerDisconnected(FIRST.peer),
                FocusEvent::ReturnToLocal,
            ] {
                assert_eq!(state.transition(event), FocusState::Local);
            }
        }
    }

    #[test]
    fn late_grant_cannot_resume_a_cancelled_request() {
        let pending = FocusState::Local.transition(FocusEvent::Request(FIRST));
        let local = pending.transition(FocusEvent::ReturnToLocal);
        assert_eq!(
            local.transition(FocusEvent::Grant(FIRST)),
            FocusState::Local
        );
        let newer = local.transition(FocusEvent::Request(NEWER));
        assert_eq!(newer.transition(FocusEvent::Grant(FIRST)), newer);
    }
}
