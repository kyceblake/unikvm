# Performance diagnostics

Add `diagnostics = true` near the top of each machine's agent config, above any `[network]` or other table. Use the default `info` log level or a more verbose level, then restart both agents. Diagnostics also work with release builds. Omit the setting or set it to `false` to turn it off.

```sh
cargo build --locked --release --workspace
```

Run the release agent with your existing `--config` path and look for `unikvm::diagnostics` in its standard error log. The log includes these measurements.

| Measurement | What it measures |
| --- | --- |
| `send_to_peer_ack_rtt` | From the sender dequeuing a sampled input event to receiving the peer's reply to the heartbeat sent immediately afterward. The peer processes the ordered input before it replies. A paced motion frame can still be waiting for its display slot. The round trip includes the return network path. |
| `receive_to_native_post` | From a decoded input event to the receiver's successful native post. For paced motions that combine several frames, the sample starts at the latest queued motion frame. This measures software scheduling and posting. Display scanout and the receiving app's handling are outside this measurement. |
| `sender_submission` | File payload bytes per second while the sender reads, hashes, and submits the payload to QUIC. Use this rate to spot sender-side constraints. |
| `receiver_verified` | File payload bytes per second from the start of receiving through staging and checksum verification. Use this as the transfer rate. |

Input logs report sample count, mean, and maximum in microseconds for keyboard, mouse motion, and mouse button or scroll. The sender samples each category at most once per second. Active sessions report about every five seconds and flush remaining samples on exit. Transfer logs report bytes, time in microseconds, and decimal MB/s. Approval and final paste into a folder happen outside the timed file transfer.

## Record a baseline

1. Record the build revision, direction, network type, and receiver display refresh rate. Use the same settings for later comparisons.
2. Use the remote mouse and keyboard for 30 seconds in each direction. Move continuously for part of the run and include mouse clicks, scrolling, and typing in a scratch window.
3. Repeat while sending the same 50,000,000-byte file in each direction. Compare the input mean and maximum against the idle run, and record the receiver's verified transfer rate. Repeat for three runs before comparing builds.

Diagnostics record categories, counts, durations, and file sizes. Keep the logs local unless you choose to share them. See the [security notes](security.md) for the data included in diagnostics.
