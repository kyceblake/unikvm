# Architecture

Each installation runs a headless user-session agent. When peer networking is available, every agent will listen for and initiate direct connections. The CLI and future TUI are local clients and may exit independently of the agent. Other agents will continue operating when a peer goes offline.

## Workspace boundaries

| Component | Responsibility |
| --- | --- |
| `apps/agent` | Starts the local endpoint, serves status, reads configuration, sets up tracing, and owns shutdown. |
| `apps/cli` | Queries local agent status and renders its PID and uptime. |
| `core` | OS-independent device IDs, peer and monitor types, events, display geometry, and focus transitions. |
| `protocol` | Versioned local status frames. The future LAN wire schema belongs here. |
| `platform` | Native IPC and stand-alone input experiments. Native handle types stay inside this boundary and its application callers. |
| `config` | TOML schema, defaults, and validation. |
| `network`, `clipboard`, `transfer` | Crate boundaries for discovery and authenticated peers, clipboard synchronization, and approved transfers. |

The platform input interfaces planned for backend implementation are `InputCapture`, `InputSink`, `HotkeyBackend`, `DisplayProvider`, `ClipboardBackend`, `ApprovalPrompt`, and `CredentialStore`. Define their contracts against working native paths as each phase needs them. The probe has no dependency on the agent.

## Core behavior

Display rectangles use logical integer coordinates and half-open bounds. Seamless handoffs use the overlapping segment of touching edges and preserve the shared desk coordinate. Explicit target mapping supports proportional placement on a different-sized edge by mapping the first and last pixels to the corresponding endpoints. A single-pixel exit edge maps to the midpoint. Monitor scale and rotation will be converted by the platform and layout layers before these calculations. Later layout selection must skip handoffs between monitors on the same machine.

Focus is a pure transition function. Outgoing control starts after a matching grant. Each desktop can accept one incoming owner while local. Physical activity revokes a receiving session, and emergency return clears any active or pending state. Disconnect, timeout, and release are represented by transitions keyed to the relevant peer or session. The agent will validate authorization and leases before dispatch and perform input cleanup on transitions.

## Conventions

Cargo uses Rust 2024, the pinned stable compiler, shared dependencies, and a workspace lockfile. Library contracts use typed errors or `io::Error` at I/O boundaries. Executables add context with `anyhow`. The agent emits structured tracing fields to stderr. Local requests use fixed frames and a two-second deadline. The agent processes one status exchange at a time. See [protocol](protocol.md), [security](security.md), and [platform support](platform-support.md).
