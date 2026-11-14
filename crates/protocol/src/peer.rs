//! First authenticated peer protocol over QUIC.

/// TLS negotiates this exact version before a connection is opened to the application.
pub const ALPN: &[u8] = b"unikvm/1";
/// Both endpoints send this fixed frame to confirm application acceptance.
pub const READY: &[u8] = b"READY";
