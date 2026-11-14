# Protocol

## Local status, version 1

The client sends the 16-byte ASCII frame `UNIKVM/1 STATUS\n`. The agent replies with the 12-byte ASCII header `UNIKVM/1 OK\n` followed by an unsigned 32-bit process ID and an unsigned 64-bit uptime in seconds, both big-endian. The client validates the frame and sends byte `0x06` to acknowledge receipt. The acknowledgment lets the pipe server close the connection after the client has read the data.

One exchange is allowed per connection. Invalid, truncated, or unsupported frames fail the exchange. On the client the two-second deadline covers connect and exchange. On the agent it starts after accepting the connection.

## LAN protocol direction

The future protobuf schema will negotiate a protocol version and capabilities, validate inbound data, and frame control messages on bounded reliable QUIC streams. File contents will use dedicated streams. Evaluate datagrams for sequenced mouse motion after profiling the coalesced reliable path. Key and button releases require reliable delivery and local cleanup on control loss.
