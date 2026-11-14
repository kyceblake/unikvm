# Progress

## 2026-11-14 — Phase 4 HTML clipboard sync

- Added optional HTML alongside the plain-text representation under the same revision. Both representations are limited to 64 KiB each. Both agents need clipboard protocol version 2.
- Windows publishes HTML Format with UTF-8 byte offsets and transfers the selected fragment. macOS reads and writes HTML and plain text on the same pasteboard item. Both retain privacy markers and echo suppression. [Microsoft HTML Clipboard Format](https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format), [Apple pasteboard items](https://developer.apple.com/documentation/appkit/nspasteboarditem)
- Windows formatting, strict Clippy, build, 56 workspace tests, and the native clipboard regression passed. Linux cross-target Clippy passed. Windows returned access denied while starting the compiler for the Apple Silicon AppKit dependencies.
- Next, update both agents and verify browser-to-editor rich-text and plain-text paste in both directions on the paired desktops.

## 2026-11-14 — Phase 4 native clipboard sync

- Added a separate `[clipboard]` opt-in for one pinned peer on Windows and macOS. A versioned readiness exchange lets two enabled agents share new text without sending whatever was on the clipboard at startup.
- Native adapters track clipboard ownership changes, skip supported sensitive-content markers, and mark imported text to avoid echo. The clipboard worker stays independent of keyboard and mouse handoff. See the [clipboard setup and paired test](clipboard-sync.md).
- Windows formatting, strict Clippy, build, and 53 workspace tests passed, including a bidirectional transfer near the 64 KiB limit over an authenticated loopback connection. The native Windows regression verified Unicode clipboard writes, suppression of received text, and rejection of a stale write. Linux cross-target Clippy and Cargo deny passed. The Apple Silicon cross-check was blocked by a Windows access-denied error when compiling the new AppKit dependencies.
- The operator confirmed live plain-text copy and paste in both directions between Windows and macOS. The Windows log also confirmed that clipboard sessions re-established after peer reconnects.
- Next, verify repeated copy/paste after reconnect, simultaneous copies, and the one-sided opt-in case on the paired desktops before expanding clipboard formats.

## 2026-11-14 — Phase 4 plain-text clipboard foundation

- Kept the fixed two-display handoff for the first release. Moved monitor enumeration, layout synchronization, and the layout editor to Phase 7 after release. Clipboard, file transfers, and release hardening are now Phases 4 through 6.
- Added a versioned plain-text update frame with a 64 KiB UTF-8 limit, authenticated-origin checks in the clipboard state, and per-session deduplication. Logical revisions break simultaneous-write ties by device ID.
- Windows formatting, strict Clippy, workspace build, and 51 workspace tests passed. Apple Silicon and x86-64 Linux passed cross-target Clippy. Cargo deny passed with existing duplicate-version warnings.
- Next, connect this format to a mutually opted-in clipboard stream and native Windows and macOS adapters. Baseline the current clipboard on connection without broadcasting it, skip entries marked sensitive by their source, and filter local notifications triggered by remote writes. Verify with bidirectional copy/paste on the paired desktops.

## 2026-11-15 — Opt-in locked Mac receiver and Windows feasibility probe

- Added `input.allow_locked_receive` (default false) for a pinned receiving peer. Mac reception requires this user's logged-in console session; Secure Input is allowed only while that eligible session is normally locked. Protected local input is never forwarded. The monitor survives suspension and recreates its complete event tap after unlock.
- The operator unlocked the Mac using Windows input and confirmed sharing resumed. Fixed a subsequent edge-return regression: the receiver retains its configured return side after unlock, and shortcut sessions close compatibly without sending an unexpected edge frame. The operator confirmed mouse crossing returns to Windows again.
- Added a capability-only Windows LocalSystem service/console-worker probe with a SYSTEM-only, PID-verified pipe and bounded reports. It never injects input. A protected-path administrative script installs, runs, reports, and removes the one-shot probe. Actual Windows locked receiving remains explicitly unsupported until native proof and a separately authenticated injector are implemented.
- Apple Silicon formatting, strict Clippy, workspace build and 46 standard tests passed. Windows platform/example cross-target strict Clippy passed; this is compile-time verification only. Read-only Mac session/permission probing passed. Native Windows installation and secure-desktop attachment remain pending operator testing.

## 2026-11-15 — Complete fast crossing before edge return

- Fixed a discarded crossing step: the receiver could report an edge return before applying the movement that reached it, leaving the visible cursor far inside the display. Both native sinks now place the cursor at the exact exit edge and interpolated crossing height before returning the handoff result. The Mac also updates its cached pointer and posts the tagged position.
- Schedule receiver hiding before the return write, so a stalled write does not delay the hide request; restore visibility if that write fails. The native monitor still applies visibility asynchronously, so strict cross-machine scanout ordering is not guaranteed.
- Apple Silicon formatting, strict Clippy, all 40 standard workspace tests, and workspace build passed. A native regression failed before the fix with the cursor 639 pixels inside the display, then passed for both horizontal edges with diagonal overshoot. The native receiver hide/restore test also passed. Windows runtime verification remains pending.
- Restarted the paired Mac client. The operator's initial fast Windows-mouse round-trip retest looked fixed; repeated physical crossings remain useful to confirm visual behavior under different speeds.

## 2026-11-14 — Hide the receiver's cursor on edge return

- The operator reported that when the Windows mouse crossed back to Windows, the Mac continued to show its cursor at the edge.
- A successful edge return now parks and hides the receiving cursor. Physical input, the next incoming handoff, or peer disconnection restores it. The native monitor also releases the hide when it stops or loses access to the desktop.
- The Windows workspace checks, six native desktop tests, and input probe passed. Apple Silicon and x86-64 Linux passed cross-target Clippy. The macOS visibility regression needs a logged-in Mac with Accessibility access.
- Next, repeat the Windows-mouse round trip on the paired desktops and touch the Mac trackpad after returning to confirm the Mac pointer reappears.

## 2026-11-14 — Lower-latency edge handoff and cursor visibility

- Send the first heartbeat with the edge request to remove a separate network round trip. Capture still starts after a matching grant and fresh acknowledgment. Initialize display pacing once the cursor is on the receiving display.
- Wake the native capture worker directly when control changes. A mapped return rearms the shared edge immediately. The sender still reads the return position if QUIC stops its write side first.
- Windows hides its parked cursor with a scoped, non-activating window. macOS continues to scope hiding to capture. The Windows desktop regression verified a blank cursor while forwarding, a visible cursor at the mapped return position, and unchanged foreground focus.
- Windows formatting, strict Clippy, workspace build, 45 workspace tests, five native desktop tests, and the input probe passed. Apple Silicon and x86-64 Linux passed cross-target strict Clippy.
- Next, verify fast back-and-forth crossings on the paired Windows and Mac desktops and check that only the active pointer remains visible during forwarding.

## 2026-11-14 — Edge handoff cursor placement

- The first paired test confirmed edge sessions and return messages. The operator reported a visible center jump during handoff.
- Removed the Windows sender's center warp and switched edge-session movement to relative Raw Input. Edge entry lands two desktop units inside the receiving edge at the normalized crossing height. The Mac publishes its entry position immediately and restores its return position before showing the cursor.
- Windows workspace checks passed. The native probe verified a quarter-height entry near the left edge without triggering local takeover. Apple Silicon and x86-64 Linux passed cross-target Clippy.
- Next, verify live crossings near the top and bottom in both directions with a relative Windows mouse and Mac trackpad.

## 2026-11-14 — Phase 4 fixed display edge

- Accepted the two-machine input MVP and added opt-in horizontal edge switching for the primary displays of a pinned bidirectional pair. Opposite `peer_side` settings are required. Crossing carries a normalized pointer height, and crossing back returns the original sender's keyboard and pointer locally.
- Windows workspace build, tests, strict Clippy, and desktop probes passed, including native pointer placement without false takeover. Apple Silicon and x86-64 Linux passed cross-target strict Clippy.
- Next, verify both crossings and the emergency shortcut on the paired desktops. The first slice uses one fixed pair of primary displays and requires an agent restart after changing the primary displays. Monitor selection, layout synchronization, and the editor remain open.

## 2026-11-14 — Japanese IME takeover on Windows

- Two Mac-to-Windows tests reproduced a false local takeover when Cmd+Space selected Japanese. Both traces showed an untagged key release at the input-language switch, and the authenticated peer connection stayed up.
- Windows keyboard takeover now requires a key press. Key releases still go through the normal capture path. The regression test verifies that an untagged release leaves the takeover signal unchanged and a press triggers it.
- Windows workspace checks and desktop input probes passed. The operator confirmed the Japanese switch worked over the patched Mac-to-Windows session.

## 2026-11-15 — macOS sender cursor drift recovery

- Fixed visible movement of the Mac sender's cursor by hiding it during capture and returning it to its saved anchor before showing it on release. Capture retains the anchor even after disassociation succeeds and corrects drift on mouse callbacks and monitor polls. Hide/show and reassociation are scoped to capture cleanup.
- Background hiding requires the private, process-local WindowServer property `SetsCursorInBackground`. Its entry points are resolved at runtime; missing support or ineffective hiding rejects capture. No system preferences or dependencies changed.
- On this Apple Silicon Mac, the native regression verified hiding, repeated drift correction, and restored visibility; all 37 standard workspace tests, formatting, strict Clippy, and workspace build passed. The operator confirmed Mac-to-Windows forwarding with the cursor hidden, then restored after Ctrl+Shift+F10.
- Further work: verify foreground changes and cleanup on other macOS versions. This private cursor capability remains a compatibility constraint.

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
