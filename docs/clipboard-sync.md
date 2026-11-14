# Share text, HTML, and images between Windows and Mac

Use a working [paired connection](manual-pairing.md) and a clipboard-enabled build on both machines. Add this table to each machine's `local-pairing/agent.toml`, using the other machine's pinned fingerprint. If you already use input sharing, copy the value from that machine's `[input]` table.

```toml
[clipboard]
peer_fingerprint = "OTHER_DEVICE_FINGERPRINT"
```

Update and restart both agents, then wait for `clipboard sync connected` in both logs. Both builds must speak clipboard protocol version 3. On macOS, allow pasteboard access if macOS prompts. With both desktops unlocked, copy a formatted paragraph from a browser on Windows. Paste it into a Mac rich-text editor and then a plain-text editor. Repeat from Mac to Windows. Clipboard sharing works independently of which machine currently receives mouse and keyboard input. [Apple pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)

The limits are 64 KiB of UTF-8 plain text and 64 KiB of optional HTML per copy. The Windows adapter transfers the selected HTML fragment, so formatting that depends on styles outside the selection can be lost. The receiving editor controls which markup it imports.

To test images, use the browser's **Copy Image** command or copy a small screenshot to the clipboard. On Mac, use **Control+Shift+Command+4** and select a region. Paste into Paint on Windows or use **File → New from Clipboard** in Preview on Mac. The wire format is PNG, limited to 8 MiB, 4096 pixels per side, and 9,437,184 pixels total. Windows reads PNG and DIB and publishes PNG and DIBV5. macOS reads PNG, TIFF, and JPEG and publishes PNG and TIFF. On Mac, a supported raster image takes priority over captions and auxiliary pasteboard items. Telegram's **Copy Media** can supply both a raster image and a file reference. UniKVM uses the supplied image data, so it can paste into an image editor. On Windows, nonempty text takes priority.

Copying a JPEG file in Finder or Explorer uses a file reference and requires Phase 5 file transfer. To copy the image from a JPEG on Mac today, open it in Preview, select the image with **Command+A**, then copy it with **Command+C**.

The agent sends new copies made after the clipboard session starts and skips entries with supported sensitive-content markers. Remove the `[clipboard]` table and restart either agent to pause sharing.
