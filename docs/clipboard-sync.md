# Share plain text between Windows and Mac

Use a working [paired connection](manual-pairing.md) and a clipboard-enabled build on both machines. Add this table to each machine's `local-pairing/agent.toml`, using the other machine's pinned fingerprint. If you already use input sharing, copy the value from that machine's `[input]` table.

```toml
[clipboard]
peer_fingerprint = "OTHER_DEVICE_FINGERPRINT"
```

Restart both agents and wait for `clipboard text sync connected` in both logs. On macOS, allow pasteboard access if macOS prompts. With both desktops unlocked, copy a fresh line of text in Windows and paste it in a Mac editor. Then copy different text on the Mac and paste in a Windows editor. Clipboard sharing works independently of which machine currently receives mouse and keyboard input. [Apple pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)

The limit is 64 KiB of UTF-8 text per copy. Files, images, and formatting are outside this slice. The agent sends new copies made after the clipboard session starts. It skips entries with supported sensitive-content markers. Remove the `[clipboard]` table and restart either agent to pause sharing.
