# Dependency decisions

Reviewed 2026-11-14. Rust 1.100.0 is the current stable release and is pinned for reproducible checks. Versions, declared MSRVs, licenses, and dates below come from the latest non-yanked stable releases on crates.io. Publication dates and upstream repositories provide maintenance signals. Recheck candidates with older releases at adoption and validate platform behavior in a running session. [Rust release announcements](https://blog.rust-lang.org/releases/)

## Adopted in the first build

| Crate | Stable release | Declared MSRV | License | Published | Decision |
| --- | --- | --- | --- | --- | --- |
| [tokio](https://crates.io/crates/tokio) | 1.54.1 | 1.85 | MIT | 2026-10-21 | Async runtime, timers, shutdown signals, named pipes and Unix sockets. |
| [windows-sys](https://crates.io/crates/windows-sys) | 0.61.2 | 1.71 | MIT OR Apache-2.0 | 2025-10-06 | Microsoft bindings for the named pipe security adapter and Windows input probe. |
| [rustix](https://crates.io/crates/rustix) | 1.1.6 | 1.65 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 2026-10-27 | Effective Unix UID and native open flags without direct FFI. |
| [serde](https://crates.io/crates/serde) | 1.0.229 | 1.56 | MIT OR Apache-2.0 | 2026-07-18 | Derive configuration deserialization. |
| [toml](https://crates.io/crates/toml) | 1.2.1+spec-1.1.0 | 1.85 | MIT OR Apache-2.0 | 2026-11-06 | Deserialize TOML and report syntax or schema errors. |
| [thiserror](https://crates.io/crates/thiserror) | 2.0.21 | 1.77 | MIT OR Apache-2.0 | 2026-09-23 | Typed configuration errors with sources. |
| [anyhow](https://crates.io/crates/anyhow) | 1.0.104 | 1.68 | MIT OR Apache-2.0 | 2026-07-18 | Application error context. |
| [tracing](https://crates.io/crates/tracing) | 0.1.44 | 1.65 | MIT | 2025-12-18 | Structured agent events. |
| [tracing-subscriber](https://crates.io/crates/tracing-subscriber) | 0.3.23 | 1.65 | MIT | 2026-03-13 | Level filtering and stderr formatting. |

Tokio's fixed buffers, timers and local transports cover the status path. On Windows, Microsoft bindings expose explicit pipe security attributes and Win32 probe APIs. On Unix, Tokio provides peer credentials and rustix supplies the current effective UID. [Tokio named pipe server](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/struct.ServerOptions.html), [Tokio Unix peer credentials](https://docs.rs/tokio/latest/src/tokio/net/unix/ucred.rs.html), [rustix effective UID](https://docs.rs/rustix/latest/rustix/process/fn.geteuid.html)

## Reviewed for later milestones

| Crate | Stable release | Declared MSRV | License | Published | Decision |
| --- | --- | --- | --- | --- | --- |
| [quinn](https://crates.io/crates/quinn) | 0.11.12 | 1.85 | MIT OR Apache-2.0 | 2026-09-14 | One QUIC endpoint can accept and dial, with streams and Rustls integration. |
| [rustls](https://crates.io/crates/rustls) | 0.23.46 | 1.71 | Apache-2.0 OR ISC OR MIT | 2026-10-14 | TLS 1.3. Review client and server certificate verifier paths before implementing pairing. |
| [rcgen](https://crates.io/crates/rcgen) | 0.14.10 | 1.88 | MIT OR Apache-2.0 | 2026-08-28 | Candidate for generating installation TLS certificates with a supported signature algorithm. |
| [ed25519-dalek](https://crates.io/crates/ed25519-dalek) | 3.0.0 | 1.85 | BSD-3-Clause | 2026-07-06 | Consider only if a separately justified signing role remains after selecting certificate identity. |
| [mdns-sd](https://crates.io/crates/mdns-sd) | 0.21.7 | 1.71 | Apache-2.0 OR MIT | 2026-11-02 | Register and browse DNS-SD records. Verify dual-stack address handling and interface changes. |
| [prost](https://crates.io/crates/prost) | 0.14.6 | 1.85 | Apache-2.0 | 2026-11-12 | Versioned protobuf LAN messages. Decide how to provide protoc reproducibly when introducing schema generation. |
| [ratatui](https://crates.io/crates/ratatui) | 0.30.2 | 1.88 | MIT | 2026-06-19 | Terminal rendering and lifecycle. Build a status view once real peer data is available. |
| [crossterm](https://crates.io/crates/crossterm) | 0.29.0 | 1.63 | MIT | 2025-04-05 | Terminal events for Ratatui. Recheck matching Ratatui backend features when wiring the TUI. |
| [enigo](https://crates.io/crates/enigo) | 0.6.1 | 1.85 | MIT | 2025-08-28 | Injection candidate. Verify tagging, physical-key mapping, and release behavior before adoption. |
| [core-graphics](https://crates.io/crates/core-graphics) | 0.25.0 | 1.65 | MIT OR Apache-2.0 | 2025-05-27 | macOS event taps and posting. Check bindings for permission preflights when prototyping. |
| [x11rb](https://crates.io/crates/x11rb) | 0.14.0 | 1.68 | MIT OR Apache-2.0 | 2026-07-16 | X11 protocol client for XInput2, XTEST and RandR, subject to runtime checks. |
| [ashpd](https://crates.io/crates/ashpd) | 0.14.0 | 1.89 | MIT | 2026-11-05 | Portal frontend for discovery of Wayland capabilities, consent and session management. |
| [reis](https://crates.io/crates/reis) | 0.7.1 | 1.71 | MIT | 2026-07-30 | Rust EI protocol implementation candidate used with a portal-provided EIS socket. |
| [clipboard-rs](https://crates.io/crates/clipboard-rs) | 0.3.5 | 1.67 | MIT | 2026-06-30 | Evaluate text and change notifications per desktop, including Wayland capability and consent. |
| [display-info](https://crates.io/crates/display-info) | 0.5.10 | Unspecified | Apache-2.0 | 2026-10-22 | Evaluate IDs, logical geometry, DPI and rotation per platform. |
| [keyring](https://crates.io/crates/keyring) | 4.2.0 | 1.88 | MIT OR Apache-2.0 | 2026-08-29 | Evaluate native credential backends and explicit store selection and availability. |
| [blake3](https://crates.io/crates/blake3) | 1.8.7 | Unspecified | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | 2026-08-20 | Streaming file integrity once transfer is in scope. |

These candidates stay outside the dependency graph until a milestone exercises them. Their release MSRVs are compatible with the pinned compiler where declared. Platform prerequisites and future transitive resolution still need validation. [Quinn endpoint](https://docs.rs/quinn/latest/quinn/struct.Endpoint.html), [Rustls](https://docs.rs/rustls/latest/rustls/), [mDNS service daemon](https://docs.rs/mdns-sd/latest/mdns_sd/struct.ServiceDaemon.html), [Ratatui terminal lifecycle](https://docs.rs/ratatui/latest/ratatui/fn.run.html)

Reviewed Lan Mouse's backend organization, including separate capture and emulation modules, its Windows hooks and injection paths, and its libei barrier setup. Its input crates are GPL-3.0-or-later. This workspace uses original probe code and published native APIs. [Lan Mouse capture source](https://github.com/feschber/lan-mouse/tree/main/input-capture), [emulation source](https://github.com/feschber/lan-mouse/tree/main/input-emulation), [license](https://github.com/feschber/lan-mouse/blob/main/LICENSE)
