# Rust Cross-Platform Software KVM — Codex Implementation Plan

## Role and objective

You are a senior Rust systems engineer. Build a production-quality, open-source **software KVM** for **Windows, macOS, and Linux** that shares keyboard, mouse, clipboard, and files over a trusted local network. Prefer existing maintained Rust crates and native OS APIs over reimplementing working infrastructure.

**Architecture requirement:** Every machine runs an autonomous daemon and can both accept and initiate connections. **There is no master, mandatory coordinator, or single point of failure.** If any peer goes offline, the remaining peers continue operating.

Work **incrementally**. Begin with a feasibility and repository foundation milestone, not an attempt to implement the whole product at once. Make small, compiling, tested changes. Document platform limitations rather than hiding them.

## Product requirements

### P0 — core functionality

1. **Peer discovery and connectivity**
   - Automatically find peers on the same LAN via mDNS/DNS-SD.
   - Every device is both a QUIC listener and a connector.
   - Pair and trust devices explicitly; auto-reconnect previously trusted devices.
   - Preserve functionality among remaining peers when one device disconnects.
   - Support manual IP/hostname entry when multicast discovery is unavailable.
   - Deduplicate simultaneous connections and support IPv4/IPv6.

2. **Keyboard and mouse sharing**
   - Switch devices with customizable global shortcuts; defaults: `Super+Shift+1/2/3` to select configured devices and `Super+Shift+0` to return to the local device. Interpret Super as Win on Windows/Linux and Command on macOS where practical.
   - Seamlessly transfer mouse **and keyboard focus together** when the cursor crosses a configured display edge.
   - Capture and suppress local physical input during remote control, without propagating an event twice.
   - Implement a configurable emergency **return-to-local** shortcut that remains effective while controlling a remote device.
   - Release held buttons/modifiers and restore local control on disconnect, timeout, crash/restart, or permission loss.
   - Prevent synthetic events from being recaptured and forwarded in loops.
   - Support mouse movement, buttons, wheel, keyboard press/release, modifiers, and practical key layout differences.

3. **Display layout**
   - Enumerate each peer's physical monitors with stable identifiers where possible, logical size, scale, rotation, and position.
   - Configure a shared virtual desk layout through a **keyboard-driven TUI**.
   - Allow displays to be positioned with directional controls and optional snapping.
   - Handle mismatched resolutions, DPI/scaling, rotations, overlapping/partial edges, and offline monitors.
   - Avoid triggering cross-device handoffs when moving between two displays on the same physical machine.

4. **Encrypted, authenticated communication**
   - Use QUIC with TLS 1.3, preferably the `quinn` ecosystem.
   - Give each installation a persistent cryptographic device identity.
   - Require user-approved pairing and authenticated identity verification (e.g., a vetted PAKE or manually compared short authentication string / fingerprint). Do **not** design a homemade authentication protocol.
   - Pin trusted peer identities/certificates, reject unknown peers, and support revocation/unpairing.
   - Keep secrets in OS credential storage where available.
   - Use separate permissions for input, clipboard, and file receiving.

5. **Clipboard synchronization**
   - Sync text first; add HTML, images, and file offers later.
   - Prevent feedback loops via origin IDs, revision IDs, and bounded deduplication.
   - Define deterministic conflict handling for simultaneous updates; do not assume synchronized clocks.
   - Support privacy controls: pause sync, content/size limits, and optional manual-only sync.
   - Never log clipboard contents.

6. **Large files with user approval**
   - File copies create metadata-only offers rather than eagerly transferring data.
   - Require explicit approval before receiving files; make the approval usable without an open TUI, via a suitable native notification/dialog helper where supported.
   - Stream data with bounded buffers over dedicated reliable QUIC streams.
   - Support multiple files, progress, cancellation, BLAKE3 integrity checks, and safe temporary/staging paths.
   - Add resumable transfers after the basic transfer path is correct.
   - Normalize filenames and prevent path traversal/symlink escape; never overwrite existing files silently.
   - Start with an approved destination folder; native cross-platform file-paste integration is a later milestone.

### P1 — operational quality

- A headless **user-session agent/daemon** independent of the TUI's lifetime.
- TUI for peer status, pairing, layout editing, keybindings, permissions, and transfer status.
- Local IPC between TUI/CLI and daemon; secure it against other local users.
- User-session autostart, diagnostics, structured logs, graceful shutdown/restart, and documented permission onboarding.
- Windows/macOS/Linux build and test coverage.

### Non-goals for v1

No remote desktop/video/audio streaming, Internet/NAT traversal, cloud accounts, arbitrary remote execution, or pre-login/lock-screen control. Do not require root/Administrator to run the entire daemon.

## Technical direction

### Rust and dependencies

- Use **Rust 2024 edition** and the current supported stable toolchain.
- **Before coding**, consult crates.io/docs.rs/official repositories to verify the **latest compatible stable versions**, maintained status, MSRV, license, and API support. Do not copy unverified version numbers from this document.
- Prefer these candidates, replacing them if verification finds a better choice:

| Concern | Preferred candidate / approach |
| --- | --- |
| Async runtime | `tokio` |
| QUIC + TLS | `quinn`, `rustls` |
| LAN discovery | `mdns-sd` |
| TUI | `ratatui`, `crossterm` |
| Input injection | `enigo` where supported; native APIs otherwise |
| Input capture / suppression | platform-specific APIs; evaluate maintained input crates and `lan-mouse` source |
| Clipboard | `clipboard-rs` where supported; native adapters as needed |
| Display enumeration | `display-info` or native OS APIs |
| Secure storage | `keyring` where available |
| Serialization | `prost` (versioned protobuf schema) |
| Device identities | reviewed Ed25519/TLS certificate solution, e.g. `ed25519-dalek` where appropriate |
| File integrity | `blake3` |
| Config | `serde`, `toml` |
| Errors | `thiserror` in libraries, `anyhow` at application boundaries |
| Diagnostics | `tracing`, `tracing-subscriber` |

**Transport:** Use reliable streams for key/button state transitions, control messages, clipboard metadata, and files. Mouse motion may use a coalesced reliable stream initially; evaluate sequenced QUIC datagrams only if latency testing shows a benefit. Never transmit key-up events on a lossy channel without a recovery protocol.

### OS abstraction and feasibility constraints

Define narrow interfaces for `InputCapture`, `InputSink`, `HotkeyBackend`, `DisplayProvider`, `ClipboardBackend`, `ApprovalPrompt`, and `CredentialStore`. Keep OS API types out of core/network/protocol crates.

| Platform | Capture / suppress | Inject | Risks |
| --- | --- | --- | --- |
| Windows | Win32 low-level hooks / Raw Input as appropriate | `SendInput` | UIPI, protected/elevated windows, key layout differences |
| macOS | CoreGraphics event taps | CoreGraphics / Enigo | Accessibility, Input Monitoring, Secure Input restrictions |
| Linux X11 | XInput/X11 capture facilities | XTest | Session differences, key layouts |
| Linux Wayland | XDG InputCapture portal + libei where supported, or compositor-specific APIs | libei/RemoteDesktop portal or supported compositor protocols | Different GNOME/KDE/wlroots support and user-consent semantics |

Study [`lan-mouse`](https://github.com/feschber/lan-mouse) for real-world Rust input backend designs, while independently checking licenses and current compatibility. Wayland support must be **capability-detected**, not promised universally. Avoid privileged evdev/uinput fallback in v1 unless isolated and explicitly authorized.

### Focus and handoff model

- **Physical input origin:** The device with the actual keyboard/mouse generating the current control session.
- **Active target:** One selected device receiving that origin's input.
- **Receiver ownership:** At most one remote controller per receiving desktop, using an explicit request/grant/lease and session ID.
- **Priority:** Local physical input or emergency return can revoke remote control immediately.
- **Edge transition:** Crossing a remote screen edge updates the active target and sends an entry coordinate normalized to the corresponding adjacent edge.
- **No relay loops:** Injected events must never be interpreted as locally generated physical events.
- **Failure handling:** Heartbeat/lease expiration, disconnect, or rejected handoff returns input locally and releases all synthetic held state.
- **Remote-to-third-device transitions:** Design the protocol to permit a physical origin to switch directly from target B to target C using known virtual layout, without making B a mandatory relay. Implement after the two-device handoff is proven.

Represent focus as a tested, explicit state machine. Keep authoritative control decisions in the state machine rather than scattered event callbacks.

### Network and trust model

- Advertise `_unikvm._udp.local` using mDNS/DNS-SD (final service name may be adjusted).
- Advertisements are unauthenticated discovery hints, **never proof of identity**.
- Use a stable, random device ID bound to a cryptographic identity, not an IP address.
- Avoid duplicate full-mesh connections using a documented deterministic dial strategy.
- Implement explicit pairing, verification, trust persistence, revocation, capability negotiation, protocol version negotiation, and reconnect with backoff.
- Do not bypass TLS certificate verification or enable blanket trust-on-first-use without an explicit user action.
- Validate message sizes, rate limits, stream counts, clipboard payloads, and file metadata.

## Workspace layout

```text
unikvm/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── apps/
│   ├── agent/src/main.rs
│   └── cli/src/
│       ├── main.rs
│       ├── commands.rs
│       └── tui/
│           ├── mod.rs
│           ├── app.rs
│           ├── events.rs
│           └── widgets/
├── crates/
│   ├── core/src/          # devices, displays, geometry, focus, shortcuts
│   ├── protocol/          # versioned wire format; protobuf definitions
│   ├── network/src/       # discovery, pairing, QUIC, peers
│   ├── platform/src/      # traits and platform-specific backends
│   │   ├── windows/
│   │   ├── macos/
│   │   └── linux/
│   ├── clipboard/src/
│   ├── transfer/src/
│   └── config/src/
├── tests/integration/
├── docs/
│   ├── architecture.md
│   ├── protocol.md
│   ├── security.md
│   └── platform-support.md
└── .github/workflows/ci.yml
```

Use a Cargo workspace with shared dependency declarations. Keep crates cohesive, not artificially granular. Use clear visibility, explicit imports, `cfg(target_os = "...")` at the platform boundary, no wildcard re-export/prelude import maze, no `utils.rs` dumping ground. Isolate unavoidable `unsafe` into small, documented OS adapter functions.

### Recommended internal message categories

- `Hello` / `Capabilities` / `PeerStatus`
- `PairingRequest` / `PairingResult` / `TrustUpdate`
- `FocusRequest` / `FocusGrant` / `FocusRelease` / `FocusRevoked`
- `KeyDown` / `KeyUp` / `PointerMove` / `PointerButton` / `Wheel`
- `DisplaySnapshot` / `LayoutUpdate`
- `ClipboardAdvertise` / `ClipboardRequest` / `ClipboardContent`
- `FileOffer` / `FileAccept` / `FileReject` / `FileChunk` or stream metadata / `FileComplete`

The schema must be versioned and forward-compatible. All fields from untrusted peers require validation. File bytes belong in streams rather than enormous protobuf messages.

## Implementation milestones and exit criteria

### Phase 0 — research and platform feasibility (first priority)

- Verify crate choices and produce a dependency decision record.
- Prototype input capture, suppression, injection, global hotkey, cursor crossing/warping, and permission checks on target OSes.
- Document a precise capability matrix for Windows, macOS, X11, GNOME Wayland, KDE Wayland, and Sway/wlroots (as tested).
- Identify which features require native APIs or user permissions.

**Done when:** The repository clearly identifies which input paths actually work and which are unsupported; there are minimal, runnable platform probes. Do **not** design a fake universal Wayland implementation.

### Phase 1 — compileable workspace and local agent

- Establish workspace, dependency versions, code style, error/logging conventions, and configuration schema.
- Implement core types: `DeviceId`, `Peer`, `MonitorId`, `DisplayRect`, `InputEvent`, `FocusState`.
- Implement local IPC with an agent exposing health and status to `unikvm status`.
- Add an initial Ratatui screen showing daemon status and discovered peers (real data, not hardcoded fixtures in the production path).

**Done when:** Supported targets build, tests pass, agent runs independently of TUI, and `cargo fmt`/Clippy pass.

### Phase 2 — discovery, pairing and mesh networking

- Implement QUIC listener/connector, mDNS discovery, manual peers, certificate identity, approved pairing, trust store, and reconnect.
- Establish direct connections among three devices; prevent duplicate sessions.
- Handle joining, leaving, rejoining, and independent communication when one node disappears.

**Done when:** A↔B↔C can establish authenticated direct peer connections; taking B offline leaves A↔C functional.

### Phase 3 — input control MVP

- Implement capture/suppression and injection for the first verified platforms.
- Add focus request/grant/release, global shortcuts, physical-event filtering, synthetic held-key tracking, and emergency local recovery.
- Start with two-device keyboard and mouse control, then multiple peers.

**Done when:** Two real machines share input bidirectionally without doubled input or stuck modifiers; killing either daemon recovers local input safely.

### Phase 4 — seamless display layout

- Enumerate monitors and synchronize virtual layout.
- Implement normalized edge mapping, neighbor lookup, cursor transitions, and keyboard focus following the pointer.
- Add an editable layout TUI with save/load and offline-display placeholders.

**Done when:** Transition works across different-sized displays, and physical native multi-monitor transitions remain local.

### Phase 5 — clipboard

- Add text synchronization with origin/revision tracking and deterministic conflict resolution.
- Add HTML and images only after text passes loop and privacy tests.
- Expose per-peer sync permissions and pause controls.

**Done when:** Repeated bidirectional copy operations do not echo indefinitely or accidentally disclose clipboard content to untrusted peers.

### Phase 6 — file offers and large transfers

- Add metadata-only clipboard file offers, recipient confirmation, chunked streaming, staging, progress, cancellation, verification.
- Test files larger than available RAM; then add resume support and native paste integration.

**Done when:** Transfers are bounded-memory, recipient-approved, verified, and never expose partial files as completed files.

### Phase 7 — hardening and releases

- Cross-platform CI, automated tests, integration test harnesses, packaging, user-session autostart, onboarding, platform capability warnings, and threat-model review.
- Performance instrumentation and optimization after functional correctness.

**Done when:** Installation, pairing, normal use, and disconnection recovery are documented and reproducible on supported environments.

## Tests and quality gates

- Unit tests: rectangle geometry, edge mappings, focus state transitions, event serialization, replay/deduplication, clipboard revision handling, hotkey interpretation.
- Integration tests: peer handshake, identity rejection, reconnect, duplicate dialing, offline node independence, lease expiration, clipboard cycles, interrupted file transfers.
- Manual hardware tests: stuck Ctrl/Shift/mouse button during disconnect; daemon crash while remote input active; simultaneous controllers; mixed DPI/rotation; multi-monitor host; UAC/elevated Windows; macOS permission revocation; GNOME/KDE/Sway compositor-specific input behavior; multi-gigabyte file transfers.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`, plus dependency/security checks (`cargo audit` and `cargo deny`, once configured).
- Favor bounded channels, cancellation-aware async tasks, no blocking OS callbacks on Tokio worker threads, and no logging of secrets, keystrokes, or clipboard contents.

## Code style and delivery instructions for Codex

1. **Do not implement all phases in one giant PR.** Follow milestones and preserve a working build.
2. Research crates and OS API support before adding dependencies. State assumptions and incompatibilities plainly.
3. Write idiomatic Rust with explicit module boundaries, small public interfaces, meaningful errors, and minimal unsafe code.
4. Keep the protocol and core free of OS-specific types.
5. Do not simulate functionality in the production path. Use platform capability flags and honest `Unsupported` errors where needed.
6. Prefer deterministic state machines and tests over timing hacks, busy loops, or unbounded queues.
7. Keep a concise progress entry in `docs/progress.md` including what changed, tests run, remaining blockers, and next step.
8. When you reach an OS-specific blocker, implement a well-defined interface and testable mock, document the blocker, and continue with portable components rather than inventing unsupported APIs.
9. Do not introduce a central controller or require a particular machine to be online.
10. Optimize only after profiling; target responsive cursor movement, low idle CPU, and bounded transfer memory.

## Start here — first Codex task

**Implement only Phase 0 and the initial skeleton of Phase 1.**

1. Inspect the repository and existing files, if any.
2. Verify maintained crates and latest compatible versions using reliable primary sources.
3. Create `docs/architecture.md`, `docs/security.md`, and `docs/platform-support.md` with explicitly labeled verified vs unverified capabilities.
4. Create the Cargo workspace and basic crates/apps shown above, with minimal compiling APIs and an agent plus CLI health/status IPC path.
5. Implement pure, thoroughly tested display geometry and focus-state types; do not wire real input injection yet.
6. Add formatting, Clippy, tests, and initial CI for Windows/macOS/Linux.
7. Include small proof-of-concept input backend probes for platforms available to you; where the OS cannot be tested, clearly mark it unverified and do not claim it works.
8. Produce a short change summary, verification commands, limitations, and an exact proposed Phase 2 task.

**Do not ask for unnecessary clarification. Make reasonable, documented design choices, and prioritize a compiling, testable repository over placeholder feature claims.**
