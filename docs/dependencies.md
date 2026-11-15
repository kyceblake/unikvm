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

## Adopted for manual peer networking

| Crate | Stable release | Declared MSRV | License | Published | Decision |
| --- | --- | --- | --- | --- | --- |
| [quinn](https://crates.io/crates/quinn) | 0.11.12 | 1.85 | MIT OR Apache-2.0 | 2026-09-14 | Tokio UDP transport with one endpoint for dialing and listening. |
| [rustls](https://crates.io/crates/rustls) | 0.23.46 | 1.71 | Apache-2.0 OR ISC OR MIT | 2026-10-14 | TLS 1.3, standard WebPKI verification in both roles, and disabled resumption. |
| [rcgen](https://crates.io/crates/rcgen) | 0.14.10 | 1.88 | MIT OR Apache-2.0 | 2026-08-28 | Generate Ed25519 certificates for client and server authentication. |
| [ring](https://crates.io/crates/ring) | 0.17.14 | 1.66 | Apache-2.0 AND ISC | 2025-03-11 | Shared cryptography provider for Quinn, Rustls, and rcgen. Reuse its SHA-256 implementation for fingerprints. |
| [tempfile](https://crates.io/crates/tempfile) | 3.27.0 | 1.63 | MIT OR Apache-2.0 | 2026-03-11 | Stage the identity locally and persist it without replacing an existing identity. |
| [zeroize](https://crates.io/crates/zeroize) | 1.9.1 | 1.85 | Apache-2.0 OR MIT | 2026-10-06 | Clear serialized private key material and DPAPI plaintext buffers after use. |

Use one crypto provider to limit the dependency and native build surface. The dependency license check now includes ISC and BSD-3-Clause for the reviewed crypto stack. Windows identity files use DPAPI through the existing Windows bindings. [Quinn endpoint](https://docs.rs/quinn/0.11.12/quinn/struct.Endpoint.html), [Rustls client certificate verifier](https://docs.rs/rustls/0.23.46/rustls/server/struct.WebPkiClientVerifier.html), [tempfile identity persistence primitive](https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist_noclobber), [Windows DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)

Phase 4 uses Tokio byte I/O and native clipboard adapters. Windows reuses `windows-sys` for sequence counts, Unicode text, and clipboard privacy formats. macOS uses `objc2` 0.6.5 (MIT), `objc2-app-kit` 0.3.2 (Zlib OR Apache-2.0 OR MIT), and `objc2-foundation` 0.3.2 (MIT). All declare Rust 1.71 as their MSRV. The AppKit adapter uses a minimal feature set for pasteboard types, change counts, and item writes. macOS may prompt for programmatic pasteboard access. [Tokio byte I/O](https://docs.rs/tokio/1.54.1/tokio/io/trait.AsyncReadExt.html), [Windows clipboard formats](https://learn.microsoft.com/en-gb/windows/desktop/dataxchg/clipboard-formats), [objc2 release metadata](https://crates.io/crates/objc2/0.6.5), [AppKit bindings](https://crates.io/crates/objc2-app-kit/0.3.2), [Foundation bindings](https://crates.io/crates/objc2-foundation/0.3.2), [Apple pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)

## Adopted for mDNS discovery

| Crate | Stable release | Declared MSRV | License | Published | Decision |
| --- | --- | --- | --- | --- | --- |
| [mdns-sd](https://crates.io/crates/mdns-sd) | 0.21.7 | 1.71 | Apache-2.0 OR MIT | 2026-11-02 | Register, browse, and remove DNS-SD records with async event delivery and interface scoping. |
| [if-addrs](https://crates.io/crates/if-addrs) | 0.15.0 | Unspecified | MIT OR BSD-3-Clause | 2026-02-08 | Reuse the discovery library's interface dependency to select the local interface and validate hints against its subnet. |

## Adopted for the macOS input probe

| Crate | Stable release | Declared MSRV | License | Published | Decision |
| --- | --- | --- | --- | --- | --- |
| [core-graphics](https://crates.io/crates/core-graphics) | 0.25.0 | 1.65 | MIT OR Apache-2.0 | 2025-05-27 | macOS-only Quartz event creation, tags, physical keycodes, and display bounds. Enable `highsierra` for scroll event creation. Use two native calls for posting permission. |

## Adopted for the clipboard monitor

The native monitor uses the existing `windows-sys` 0.61.2 bindings for a Win32 window, standard text controls, and painting. On macOS it enables window, text, progress indicator, and timer APIs in the existing `objc2-app-kit` and `objc2-foundation` 0.3.2 bindings. These features pull in `objc2-core-foundation` 0.3.2 and resolve optional `dispatch2` 0.3.1 in the lockfile. Both declare Rust 1.71 and Zlib OR Apache-2.0 OR MIT. The CLI reads transfer status through native local IPC. [Win32 window management](https://learn.microsoft.com/en-us/windows/win32/winmsg/windows), [AppKit bindings](https://docs.rs/objc2-app-kit/0.3.2/objc2_app_kit/), [Core Foundation bindings](https://crates.io/crates/objc2-core-foundation/0.3.2), [Grand Central Dispatch bindings](https://crates.io/crates/dispatch2/0.3.1)

The tray uses the existing AppKit bindings for the status item and transfer panel and `windows-sys` for the notification icon. Windows Shell progress uses Microsoft’s generated COM bindings in `windows` 0.62.2 (Rust 1.82, MIT OR Apache-2.0). The CLI embeds the Common Controls v6 manifest with `embed-manifest` 1.5.1 (Rust 1.85, MIT), which supports the Windows GNU and MSVC linkers. [Windows COM bindings](https://docs.rs/crate/windows/0.62.2/source/Cargo.toml.orig) · [Manifest embedding](https://docs.rs/embed-manifest/1.5.1/embed_manifest/)

The optional file drag probe enables OLE drop targets and AppKit dragging in the existing platform bindings. It declares the already locked `windows-core` 0.62.2 (Rust 1.82, MIT OR Apache-2.0) directly because the OLE implementation macro references it. [Windows Core manifest](https://docs.rs/crate/windows-core/0.62.2/source/Cargo.toml)

The Windows drop helper reuses the locked Shell and OLE bindings from the probe. The CLI depends on the workspace's `unikvm-core` for the existing `Edge` type. [Windows Shell transfer APIs](https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios)

The Finder edge helper uses the already locked AppKit drag pasteboard and window APIs. The Mac input monitor uses its existing Quartz event bindings to end the local drag once the receiving desktop is armed. [AppKit drag pasteboard](https://developer.apple.com/documentation/appkit/nsdragginginfo/draggingpasteboard)

The Finder drop helper uses `block2` 0.6.2 (Rust 1.71, MIT), maintained with the existing `objc2` bindings. The macOS-only dependency handles the AppKit file promise completion block and the synchronous Foundation file-coordination block. It matches the version range in the locked AppKit and Foundation bindings. [block2 manifest](https://docs.rs/crate/block2/0.6.2/source/Cargo.toml) · [AppKit promise delegate](https://developer.apple.com/documentation/appkit/nsfilepromiseproviderdelegate)

## Reviewed for later milestones

| Crate | Stable release | Declared MSRV | License | Published | Decision |
| --- | --- | --- | --- | --- | --- |
| [ed25519-dalek](https://crates.io/crates/ed25519-dalek) | 3.0.0 | 1.85 | BSD-3-Clause | 2026-07-06 | Consider only if a separately justified signing role remains after selecting certificate identity. |
| [prost](https://crates.io/crates/prost) | 0.14.6 | 1.85 | Apache-2.0 | 2026-11-12 | Versioned protobuf LAN messages. Decide how to provide protoc reproducibly when introducing schema generation. |
| [ratatui](https://crates.io/crates/ratatui) | 0.30.2 | 1.88 | MIT | 2026-06-19 | Terminal rendering and lifecycle. Build a status view once real peer data is available. |
| [crossterm](https://crates.io/crates/crossterm) | 0.29.0 | 1.63 | MIT | 2025-04-05 | Terminal events for Ratatui. Recheck matching Ratatui backend features when wiring the TUI. |
| [enigo](https://crates.io/crates/enigo) | 0.6.1 | 1.85 | MIT | 2025-08-28 | Injection candidate. Verify tagging, physical-key mapping, and release behavior before adoption. |
| [x11rb](https://crates.io/crates/x11rb) | 0.14.0 | 1.68 | MIT OR Apache-2.0 | 2026-07-16 | X11 protocol client for XInput2, XTEST and RandR, subject to runtime checks. |
| [ashpd](https://crates.io/crates/ashpd) | 0.14.0 | 1.89 | MIT | 2026-11-05 | Portal frontend for discovery of Wayland capabilities, consent and session management. |
| [reis](https://crates.io/crates/reis) | 0.7.1 | 1.71 | MIT | 2026-07-30 | Rust EI protocol implementation candidate used with a portal-provided EIS socket. |
| [clipboard-rs](https://crates.io/crates/clipboard-rs) | 0.3.5 | 1.67 | MIT | 2026-06-30 | Evaluate text and change notifications per desktop, including Wayland capability and consent. |
| [display-info](https://crates.io/crates/display-info) | 0.5.10 | Unspecified | Apache-2.0 | 2026-10-22 | Evaluate IDs, logical geometry, DPI and rotation per platform. |
| [keyring](https://crates.io/crates/keyring) | 4.2.0 | 1.88 | MIT OR Apache-2.0 | 2026-08-29 | Evaluate native credential backends and explicit store selection and availability. |

## Adopted for file transfers

Clipboard v5 uses [BLAKE3 1.8.7](https://crates.io/crates/blake3/1.8.7) with the `pure` feature to verify the transferred byte stream incrementally. The release declares no MSRV and is licensed CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception. Quinn, Tokio, and the native platform bindings retain their locked versions. [BLAKE3 streaming API](https://docs.rs/blake3/1.8.7/blake3/struct.Hasher.html)

These candidates stay outside the dependency graph until a milestone exercises them. Their release MSRVs are compatible with the pinned compiler where declared. Platform prerequisites and future transitive resolution still need validation. [Quinn endpoint](https://docs.rs/quinn/latest/quinn/struct.Endpoint.html), [Rustls](https://docs.rs/rustls/latest/rustls/), [mDNS service daemon](https://docs.rs/mdns-sd/latest/mdns_sd/struct.ServiceDaemon.html), [Ratatui terminal lifecycle](https://docs.rs/ratatui/latest/ratatui/fn.run.html)

## Adopted for macOS capture

| Crate | Stable release | Declared MSRV | License | Decision |
| --- | --- | --- | --- | --- |
| [core-foundation](https://docs.rs/crate/core-foundation/0.10.1/source/Cargo.toml) | 0.10.1 | 1.65 | MIT OR Apache-2.0 | Use the run loop and owned Mach port bindings to drive a session event tap. Already locked via core-graphics. |
| [foreign-types](https://docs.rs/crate/foreign-types/0.5.0) | 0.5.0 | Unspecified | MIT OR Apache-2.0 | Use the same Quartz pointer wrapper traits as core-graphics. Already locked via core-graphics. |

The event tap callback handles nullable disable notifications before wrapping a live event pointer. The pinned toolchain checks both bindings on the Apple Silicon target. [Apple event tap API](https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate(tap:place:options:eventsofinterest:callback:userinfo:))

Reviewed Lan Mouse's backend organization, including separate capture and emulation modules, its Windows hooks and injection paths, and its libei barrier setup. Its input crates are GPL-3.0-or-later. This workspace uses original native adapter code. macOS background cursor visibility additionally uses the private WindowServer connection property described below. [Lan Mouse capture source](https://github.com/feschber/lan-mouse/tree/main/input-capture), [emulation source](https://github.com/feschber/lan-mouse/tree/main/input-emulation), [license](https://github.com/feschber/lan-mouse/blob/main/LICENSE)

## macOS background cursor compatibility

The optional locked-receiver work adds no crates or version changes. macOS reads its own session dictionary through CoreGraphics/CoreFoundation and uses a narrowly checked lock-state key; other users' or inactive sessions fail closed. The capability-only Windows probe enables `Win32_System_Services`, `Win32_System_RemoteDesktop`, `Win32_System_Pipes`, and `Win32_System_EventLog` on the already locked `windows-sys` version to test an isolated SYSTEM console worker. It is not the locked-desktop input backend.

Public cursor hiding and disassociation are foreground-oriented. The headless sender uses `SetsCursorInBackground` on its own WindowServer connection, then balances public Quartz hide/show calls during capture. This private property is also used by maintained KVM implementations; our adapter resolves the two private entry points at runtime and rejects capture if they are absent or hiding is ineffective. It restores the process-local property after showing the cursor. It neither writes persistent system preferences nor adds a package dependency. This path passed native visibility/restoration and paired desktop tests on the target Mac; future macOS releases require verification. [Lan Mouse native cursor setup](https://github.com/feschber/lan-mouse/blob/main/input-capture/src/macos.rs), [Input Leap macOS adapter](https://github.com/input-leap/input-leap/blob/master/src/lib/platform/OSXScreen.mm)
## Adopted for clipboard images

`image` 0.25.10 declares Rust 1.88 and MIT OR Apache-2.0. Enable PNG on both desktops, BMP decoding on Windows, and TIFF on macOS. The adapter uses the crate's decoder limits plus explicit pixel and transport bounds. [Image crate release](https://crates.io/crates/image/0.25.10), [Image decoding limits](https://docs.rs/image/0.25.10/image/struct.Limits.html)
