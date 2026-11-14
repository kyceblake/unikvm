# Pair a Windows PC and a Mac

This milestone opens an authenticated QUIC connection between two manually configured agents. Each agent logs authenticated connection, disconnection, and reconnect. Keep both devices on the same trusted LAN and use their LAN IP addresses. Local status continues to report the agent's PID and uptime.

## Prepare each machine

Build from the same revision on both machines using the [repository setup](../README.md). Create an ignored `local-pairing` directory from the repository root. Run the following on both machines. Identity creation prints the local SHA-256 fingerprint.

```sh
mkdir local-pairing
cargo run --locked -p unikvm-cli -- identity init local-pairing/identity.bin
cargo run --locked -p unikvm-cli -- identity export local-pairing/identity.bin local-pairing/local.der
```

Copy each machine's public `local.der` to the other machine as `local-pairing/peer.der`. Keep `identity.bin` on the machine that created it. Compare the full 64-character fingerprint directly with the value printed on the source machine or obtained through another trusted channel. You can display it again with `identity fingerprint local-pairing/identity.bin`.

Verify the imported certificate on each machine. Replace `PEER_FINGERPRINT` with the verified fingerprint of the other machine.

```sh
cargo run --locked -p unikvm-cli -- peer verify local-pairing/peer.der PEER_FINGERPRINT
```

## Connect the pair

Create `local-pairing/agent.toml` on each machine. Substitute the local and peer LAN addresses. The example assumes the local machine is `192.168.1.10`. Reverse the addresses on `192.168.1.20`. Allow incoming UDP on the configured port on both machines if the OS firewall prompts.

```toml
schema_version = 1

[network]
listen = "192.168.1.10:24800"
peer = "192.168.1.20:24800"
identity = "identity.bin"
peer_certificate = "peer.der"
peer_fingerprint = "PEER_FINGERPRINT"
```

Launch the agent on each machine.

```sh
cargo run --locked -p unikvm-agent -- --config local-pairing/agent.toml
```

Both terminals should log `authenticated peer connected`. Stop one agent with Ctrl+C and start it again with the same configuration and identity. Each agent should log another authenticated connection. If neither connects, check that the reciprocal IPs and UDP ports are correct. Set `log_level = "debug"` above `[network]` to see connection failures.

Remove the `[network]` table and restart an agent to revoke its local approval. Restore networking by explicitly verifying and configuring an approved peer again. Changing a LAN address requires updating the configuration. Identity rotation requires repeating fingerprint approval on the other machine.

## Discover the paired address

After manual pairing works, add `discovery = true` under `[network]` on both machines. Keep the certificate and fingerprint entries. The agents advertise and browse `_unikvm._udp.local.` on the interface selected by their specific `listen` address.

You can keep `peer` as an explicit address. The dialer tries it first, then tries discovered addresses if the connection fails. To test discovery by itself, remove the `peer` line from both configs and restart the agents. With debug logging enabled, look for `updated mDNS address hints for configured peer` on the dialer and `authenticated peer connected` on both machines.

Discovery needs local multicast DNS on UDP 5353 and runs on one configured address family and interface at a time. The QUIC connection still needs its own UDP port. When multicast is unavailable, restore the `peer` entry and set `discovery = false`. If this machine's own address changes, update `listen`. Discovery can learn the other machine's new address once it advertises there.
