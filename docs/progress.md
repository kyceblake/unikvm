# Progress

## 2026-11-14 — Display-paced pointer motion

- The paired desktops passed Windows-to-Mac input. Mac-to-Windows input worked with a moving local Mac pointer and visibly slower motion on the 240 Hz Windows monitor.
- Each sender forwards pointer movement directly from its native callback. macOS disassociates or parks its cursor while sending. Both receivers pace motion to the reported rate of their display and apply pending movement before a click. The input wire format is unchanged.
- The Windows desktop probe detected 240 Hz and moved the pointer in paced steps. The 60-to-240 and 240-to-60 Hz tests verify interpolation, coalescing, and preservation of relative motion. The next step on macOS is to run the [refresh probe](platform-support.md#run-the-macos-input-probes) and check cursor parking and motion in both directions on the paired desktops.

## 2026-11-14 — Bidirectional Windows and macOS input

- Added `[input] mode = "bidirectional"` for a selected pinned peer. Each machine reserves local focus for one input session. Physical keyboard, mouse, or trackpad activity on the receiver ends sharing and restores local control. The shortcut starts the next session in either direction.
- Implemented macOS capture through a Quartz session tap and Windows injection through `SendInput`. Both adapters filter synthetic events. The receiver attempts to release held keys and buttons before it grants another session.
- Windows formatting, strict Clippy, workspace build, all 36 standard tests, both native input adapter tests, and the standalone Windows input probe passed. Strict Clippy passed for Apple Silicon and x86-64 Linux as compile-time checks. Cargo deny and the cached RustSec audit passed.
- Next, run the [Mac capture probe](platform-support.md#run-the-macos-input-probes) and the [two-desktop handoff walkthrough](input-control.md). Verify trackpad motion, takeover while a remote modifier or mouse button is held, and recovery after stopping either agent. Native macOS capture, hard receiver termination with held input, and display-edge switching remain open.

## 2026-11-14 — Opt-in Windows-to-Mac input session

- The Windows operator verified physical F9 suppression and Ctrl+Shift+F10 delivery. The Mac injection probe had already passed on the target desktop.
- Added one explicit input peer per agent, a reliable QUIC input stream with a grant and renewable lease, Windows capture with a bounded queue and local watchdog, and Mac synthetic key and button release on stream cleanup. Ctrl+Shift+F9 starts a session and Ctrl+Shift+F10 returns to Windows.
- Windows formatting, strict Clippy, workspace build, and all 32 standard tests passed. The input stream test exercised both QUIC connection roles. The Windows hook installation and mDNS LAN tests passed separately. Cross-target strict Clippy passed for Apple Silicon macOS and x86-64 Linux.
- The operator verified the Windows-to-Mac session on the paired desktops. Held-input cleanup after hard termination still needs verification.

## 2026-11-14 — Phase 3 native input feasibility

- Added a macOS Quartz sink for physical key positions, modifiers, pointer motion, mouse buttons, and scroll. The standalone probe checks posting permission and exercises keyboard input, Shift release, cursor movement, and scrolling on request.
- Extended the Windows probe with an explicit, ten-second physical F9 suppression check and an independent Ctrl+Shift+F10 completion shortcut.
- Windows formatting, strict Clippy, build, all 27 standard workspace tests, and the synthetic Windows input probe passed. Cross-target Clippy passed for Apple Silicon macOS and x86-64 Linux. Cargo deny and the cached RustSec audit passed.
- The Mac operator confirmed the standalone injection exercise passed. The subsequent [Windows physical suppression probe](platform-support.md#run-the-windows-probe) also passed.

## 2026-11-14 — Multiple trusted peers

- Verified PC-to-Mac discovery with the Windows dialer configured without a manual address. Its log recorded the Mac's mDNS address and an authenticated QUIC connection.
- Added multiple approved peers per agent on one QUIC endpoint. Each pair has a deterministic dial direction, one accepted session, and independent reconnect. Existing single-peer configs still load.
- Windows formatting, strict Clippy, workspace build, and 27 standard tests passed. The three-agent loopback test established all three direct pairwise connections, kept the surviving pair connected while the third restarted, and verified both of its connections returned. Duplicate-session and unknown-certificate rejection passed. The Windows multicast test also passed with the updated transport. Cross-target Clippy passed for Apple Silicon macOS and x86-64 Linux.
- The existing Windows–Mac agents are still on the verified discovery build. Next, run the mesh revision on both machines and validate reciprocal connections with a third instance. The next implementation milestone is Phase 3 input control, starting with native input probes and the shortcut-based return-to-local flow.

## 2026-11-14 — mDNS address discovery for an approved peer

- Verified manual pairing on the Windows PC and Mac over the LAN. The Windows agent reported an authenticated connection to the Mac, and the Mac operator confirmed its side.
- Added opt-in DNS-SD on the interface selected by the agent's listen address. The dialer can use mDNS hints for the already pinned peer, alongside an optional manual address. Mutual TLS and explicit certificate approval still govern trust.
- Windows formatting, strict Clippy, workspace build, and 25 standard tests passed. A separate Windows LAN test connected two local QUIC endpoints using multicast DNS alone. Dependency policy and RustSec checks passed. Cross-target Clippy covered Apple Silicon macOS and x86-64 Linux.
- Next, run the [discovery-only walkthrough](manual-pairing.md#discover-the-paired-address) on the Windows PC and Mac. Follow with support for multiple trusted peers and a three-agent continuity test.

## 2026-11-14 — First Phase 2 networking slice

- Pulled the macOS IPC fix at `af268b0`. GitHub CI passed on Windows, macOS, and Linux for that revision.
- Added manually configured two-device connections with mutual TLS 1.3, persistent Ed25519 identities, bilateral SHA-256 fingerprint approval, and automatic reconnect. One certificate per peer and the local config keep trust explicit across restarts.
- Windows GNU passed formatting, strict Clippy, all 22 workspace tests, and build. Cross-target Clippy passed for Apple Silicon macOS and x86-64 Linux. Loopback tests covered both connection directions, unknown client and server rejection, and automatic reconnect after the listener restarted at the same address. A Windows CLI/agent smoke test verified identity creation, export, mismatched fingerprint rejection, and local status with networking enabled. Cargo deny and the cached RustSec audit passed with duplicate transitive version warnings from cargo deny.
- The new networking code still needs macOS and Linux runtime verification. Next, run the [manual pairing walkthrough](manual-pairing.md) between Windows and macOS, including stopping and restarting either machine's agent. Then add mDNS discovery, multiple trusted peers, and a three-agent continuity test. Keyboard and mouse control remains a later milestone.

## 2026-11-14 — macOS local IPC recovery

- Fixed the Unix listener terminating when macOS reports `NotConnected` during peer credential lookup for a client that closed before accept. The listener discards the socket and continues; unverified streams are never served.
- Updated the native transport regression to cover disconnects both before and after accept, followed by repeated successful status queries. Linux may authenticate the abandoned connection and observe EOF instead.
- Native verification on macOS 27.3 (arm64): formatting, Clippy with warnings denied, all 16 workspace tests, and workspace build passed. Separate agent/CLI processes returned status after three clients connected and disconnected without requests.
- Remaining limitations: macOS input probes and other desktop runtime validation remain pending. Next step remains the proposed Phase 2 networking task below.

## 2026-11-14 — Phase 0 and initial Phase 1

- Recorded dependency decisions and a six-desktop feasibility matrix. Added a standalone Windows probe. Desktop access, hotkey registration, cursor placement at the monitor edge, and tagged keyboard and mouse injection, capture, and suppression passed.
- Established a Rust 2024 workspace with a pinned stable toolchain, OS-independent core types, tested geometry and focus transitions, and a versioned TOML config. Added CI for Linux, macOS, and Windows.
- Implemented a headless agent and `unikvm status` over native local IPC. Windows uses a logon-scoped named pipe and Linux and macOS use per-user Unix sockets. A successful query returns the agent's actual PID and uptime.

### Verification

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
cargo build --locked --workspace
cargo run --locked -p unikvm-platform --example input_probe
cargo deny --offline check
cargo audit --no-fetch --no-yanked
```

Windows GNU build, formatting, Clippy, and all 16 tests passed. Separate agent and CLI processes passed repeated status queries, duplicate-agent rejection, shutdown detection after process termination, and restart. Cross-target Clippy passed for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`. These checks type-check the code and tests for those targets. Native CI runs are still pending.

Dependency checks used fetched advisory data. Cargo deny passed advisories, yanked versions, dependency sources and third-party licenses, with a warning for the two transitive `syn` major versions. Cargo audit passed its advisory scan with yanked-version checking delegated to cargo deny. Workspace packages are unpublished. Third-party license checks use an allowlist, and the project license choice is open.

### Remaining work and next step

Run the physical-input and hotkey test on Windows, including higher-integrity and mixed-DPI cases. Run native IPC tests and targeted desktop probes on macOS, X11, GNOME Wayland, KDE Wayland, and Sway. Phase 0 runtime validation remains open for those desktops. The initial Phase 1 scope ends at CLI status. The Ratatui peer view needs a real peer feed.

**Proposed first Phase 2 task.** Implement a two-agent integration test around a Quinn endpoint per agent and manually configured loopback addresses. Provision a persistent random device ID and a TLS identity per agent. Use an explicit fingerprint comparison and bilateral pinning flow. Verify that each agent can dial and accept, that an untrusted peer is rejected, and that a trusted peer reconnects after restart. Follow with mDNS discovery, simultaneous-dial deduplication, a three-agent continuity test, and a real peer feed for the TUI.
