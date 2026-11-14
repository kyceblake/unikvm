# Progress

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
