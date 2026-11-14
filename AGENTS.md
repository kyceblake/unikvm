# Working on unikvm

Read [CODEX_PLAN.md](CODEX_PLAN.md) for the architecture and milestone exit criteria, then check [docs/progress.md](docs/progress.md) for the current state. Keep changes within the requested milestone. Update the progress log when advancing a milestone, including checks run, remaining limitations, and the next step.

## Architecture and code

- Keep the agent headless and independent of the CLI and future TUI. Each agent must be able to accept and initiate peer connections without a central coordinator.
- Keep device, geometry, input, and focus logic in `crates/core`, wire formats in `crates/protocol`, and native APIs in `crates/platform`. Keep OS types out of core, protocol, and network contracts.
- Use Rust 2024 and the toolchain pinned in `rust-toolchain.toml`. Declare shared dependencies in the workspace manifest and keep `Cargo.lock` current. Verify maintenance, MSRV, license, and required APIs before adding a dependency. Record decisions in [docs/dependencies.md](docs/dependencies.md).
- Prefer small, explicit interfaces and deterministic state transitions. Keep unsafe code inside documented native adapters. Use typed library errors or `io::Error` at I/O boundaries, and add application context with `anyhow`.
- Report unsupported capabilities explicitly. Keep native probes separate from the agent. Document runtime results by OS and desktop in [docs/platform-support.md](docs/platform-support.md), and label cross-target checks as compile-time verification.
- Follow [docs/security.md](docs/security.md) when changing trust or IPC. Keep keystrokes, clipboard contents, and secrets out of logs.

## Verification

Run these commands from the workspace root for code changes.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --workspace
```

Run `cargo deny check` when changing dependencies. For changes to local IPC, launch `cargo run --locked -p unikvm-agent` and run `cargo run --locked -p unikvm-cli -- status` from another terminal in the same user session. Use the documented desktop probes for native input changes.
