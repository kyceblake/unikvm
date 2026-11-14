# Security

The current agent exposes one read-only health/status query over native local IPC. A valid response confirms that the process served the query. The response contains the process ID and uptime in whole seconds.

- Windows names its pipe using the logon SID, sets an explicit DACL for that SID, and rejects remote clients. The CLI opens the pipe at identification-only impersonation level and checks that the pipe owner matches its user SID before sending a request. First-instance creation prevents concurrent agents on the same endpoint. Other accounts guessing and precreating a pipe name can cause denial of service. The owner check rejects a forged response from such an account. [Microsoft named pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- Linux and macOS use a socket in a user-owned mode-0700 directory. Both endpoints compare effective user IDs using native peer credentials. A lock serializes binding, stale socket removal, and shutdown cleanup. The fallback under `/tmp` can be precreated by another local user, resulting in a startup failure. The ownership check fails closed. [Tokio peer credential implementation](https://docs.rs/tokio/latest/src/tokio/net/unix/ucred.rs.html)
- The agent bounds each exchange by time and frame size. A stalled local client can delay other local requests for up to one exchange deadline per connection. Processes with the same user or logon rights and OS administrators are inside the local trust boundary.

## Manual peer trust

The opt-in QUIC endpoint uses TLS 1.3 and requires client certificates. On each machine the configured SHA-256 fingerprint must match the imported public certificate. Rustls validates both sides against the configured certificate, and the application requires an exact match of the presented leaf before exchanging readiness frames. TLS resumption is disabled so a reconnect presents certificates again. The source IP filter limits incoming attempts. The certificate identifies the peer.

Installation identities use self-signed Ed25519 certificates. The device ID is the first 128 bits of the certificate digest. Trust uses the entire SHA-256 fingerprint. Windows protects the private identity file using current-user DPAPI. macOS and Linux use an owned file with mode 0600 or stricter. macOS Keychain and Linux Secret Service integration are future work. Use a directory that only the account owner can modify.

## LAN requirements for later phases

Treat mDNS, IP addresses, hostnames, and advertised IDs as untrusted hints. Pairing requires explicit approval and verification of both peer identities using a reviewed certificate fingerprint workflow or vetted PAKE. Bind a persistent random device ID to the verified certificate identity. Reconnect only against pinned trust and support revocation. Give input, clipboard, and file receipt separate permissions.

Use reliable streams for key and button state and bound every message, stream, and queue. Leases and disconnect paths must restore local control and release held synthetic state. Logs may contain operational state and counts. Keep key data, clipboard contents, and secrets out of logs.

Stream approved files to a bounded staging area. Validate filenames and destination paths, defend against traversal and symlink escape, check content integrity, and publish without silently overwriting a file.
