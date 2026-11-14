# Protocol

## Local status, version 1

The client sends the 16-byte ASCII frame `UNIKVM/1 STATUS\n`. The agent replies with the 12-byte ASCII header `UNIKVM/1 OK\n` followed by an unsigned 32-bit process ID and an unsigned 64-bit uptime in seconds, both big-endian. The client validates the frame and sends byte `0x06` to acknowledge receipt. The acknowledgment lets the pipe server close the connection after the client has read the data.

One exchange is allowed per connection. Invalid, truncated, or unsupported frames fail the exchange. On the client the two-second deadline covers connect and exchange. On the agent it starts after accepting the connection.

## Peer readiness, version 1

QUIC uses TLS 1.3 with mutual certificate authentication and ALPN `unikvm/1`. After verifying the peer's exact certificate, each endpoint opens one unidirectional stream, sends the five-byte ASCII frame `READY`, and finishes the stream. Each endpoint accepts the connection for application use after it has read the complete matching frame. The readiness exchange shares a five-second deadline with the TLS handshake.

## LAN protocol direction

For address discovery, agents opt in to advertising and browsing `_unikvm._udp.local.`. The first 128 bits of the certificate fingerprint, in lowercase hex, form the instance name. TXT records contain `version=1` and `fingerprint` with the full SHA-256 certificate digest. SRV and address records provide the candidate UDP endpoint. The dialer considers at most four discovered addresses from the directly connected subnet and selected interface. mDNS records are unauthenticated.

The future protobuf schema will negotiate a protocol version and capabilities, validate inbound data, and frame control messages on bounded reliable QUIC streams. File contents will use dedicated streams. Evaluate datagrams for sequenced mouse motion after profiling the coalesced reliable path. Key and button releases require reliable delivery and local cleanup on control loss.
