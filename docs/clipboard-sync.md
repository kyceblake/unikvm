# Share text and HTML between Windows and Mac

Use a working [paired connection](manual-pairing.md) and a clipboard-enabled build on both machines. Add this table to each machine's `local-pairing/agent.toml`, using the other machine's pinned fingerprint. If you already use input sharing, copy the value from that machine's `[input]` table.

```toml
[clipboard]
peer_fingerprint = "OTHER_DEVICE_FINGERPRINT"
```

Update and restart both agents, then wait for `clipboard sync connected` in both logs. Both builds must speak clipboard protocol version 2. On macOS, allow pasteboard access if macOS prompts. With both desktops unlocked, copy a formatted paragraph from a browser on Windows. Paste it into a Mac rich-text editor and then a plain-text editor. Repeat from Mac to Windows. Clipboard sharing works independently of which machine currently receives mouse and keyboard input. [Apple pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)

The limits are 64 KiB of UTF-8 plain text and 64 KiB of optional HTML per copy. The agent requires a plain-text representation and falls back to it when HTML is absent, invalid, or too large. The Windows adapter transfers the selected HTML fragment, so formatting that depends on styles outside the selection can be lost. The receiving editor controls which markup it imports. RTF, files, and images are outside this slice. The agent sends new copies made after the clipboard session starts and skips entries with supported sensitive-content markers. Remove the `[clipboard]` table and restart either agent to pause sharing.
