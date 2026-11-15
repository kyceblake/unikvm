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

Input logs report sample count, mean, maximum, and the estimated 95th percentile upper bound in microseconds for keyboard, mouse motion, and mouse button or scroll. `budget_us=5000` sets the 5 ms comparison and `over_budget` counts samples strictly above it. The 95th percentile uses 250 µs buckets through 31.75 ms and reports the observed maximum as the upper bound for larger stalls. Short windows with few samples can put the 95th percentile in the same bucket as the maximum. The sender samples each category at most four times per second. Active sessions report about every five seconds and flush remaining samples on exit.

Each row also breaks down its slowest sample.

| Field | What it measures |
| --- | --- |
| `worst_submit_us` | Time to submit the input event and its heartbeat to QUIC. |
| `worst_after_submit_us` | Time from submission to observing the acknowledgment. This includes transport and receiver work. |
| `worst_before_post_us` | Receiver time before entering the native input path. This can include queuing for display pacing or flushing pending motion before a key or click. |
| `worst_post_us` | Time spent in the native input path until it returns. |
| `worst_zero_steps` | Paced steps that produced no movement between receipt and the first native post of the sampled motion. |
| `worst_tick_late_us` | Largest time past a scheduled tick when the pacer resumed during that same motion sample. This is part of the time before posting. |

Transfer logs report bytes, time in microseconds, and decimal MB/s. Approval and final paste into a folder happen outside the timed file transfer.

## Record a baseline

1. Record the build revision, direction, network type, and receiver display refresh rate. Use the same settings for later comparisons.
2. Use the remote mouse and keyboard for 30 seconds in each direction. Move continuously for part of the run and include mouse clicks, scrolling, and typing in a scratch window.
3. Repeat while sending the same 50,000,000-byte file in each direction. Compare the mean, maximum, 95th percentile, and share above 5 ms for each input measurement against the idle run. Record the receiver's verified transfer rate. Repeat for three runs before comparing builds.

Diagnostics record categories, counts, durations, and file sizes. Keep the logs local unless you choose to share them. See the [security notes](security.md) for the data included in diagnostics.
