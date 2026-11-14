# unikvm

unikvm is a Rust 2024 workspace for a peer-to-peer software KVM. The headless agent provides local status IPC and opt-in, authenticated QUIC connections to explicitly paired peers. Agents can use manually entered addresses or discover their peers over mDNS. The Windows input feasibility probe runs independently.

Install Rust with [rustup](https://rustup.rs/) and use the platform's native C toolchain. On macOS, install Apple's Command Line Tools with `xcode-select --install`. On Windows, the default MSVC toolchain requires Visual Studio Build Tools with **Desktop development with C++**. The GNU toolchain requires a compatible MinGW C compiler and binutils.

From the workspace root, build and launch the agent in one terminal.

```sh
cargo build --locked --workspace
cargo run --locked -p unikvm-agent
```

In another terminal, query the agent.

```sh
cargo run --locked -p unikvm-cli -- status
```

Run both commands in the same Windows logon session or as the same Unix user. On Linux and macOS, both processes should see the same `XDG_RUNTIME_DIR`. If it is unset, the agent uses a user-owned directory under `/tmp`.

Use Ctrl+C in the agent terminal to stop it. To override the default info log level, pass `--config PATH` after Cargo's `--` separator. The configuration file accepts:

```toml
schema_version = 1
log_level = "info"
```

For two-device networking, follow [manual pairing](docs/manual-pairing.md). See [progress and verification](docs/progress.md), [architecture](docs/architecture.md), [platform support and probe commands](docs/platform-support.md), and [dependency decisions](docs/dependencies.md).
