/// A stable installation identifier. Identity generation and authentication belong to pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(pub [u8; 16]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub id: DeviceId,
    pub display_name: String,
}

/// An identifier scoped to a device. Stability depends on its display backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MonitorId(pub String);
