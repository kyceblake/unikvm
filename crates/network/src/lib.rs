//! Persistent certificate identities and authenticated peer transport.

mod transport;

pub mod identity;

pub use transport::{BindError, PeerEndpoint, PeerEvent};
