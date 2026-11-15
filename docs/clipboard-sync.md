# Share text, HTML, and images between Windows and Mac

Use a working [paired connection](manual-pairing.md) and a clipboard-enabled build on both machines. Add this table to each machine's `local-pairing/agent.toml`, using the other machine's pinned fingerprint. If you already use input sharing, copy the value from that machine's `[input]` table.

```toml
[clipboard]
peer_fingerprint = "OTHER_DEVICE_FINGERPRINT"
```

Update and restart both agents, then wait for `clipboard sync connected` in both logs. Both builds must speak clipboard protocol version 4. On macOS, allow pasteboard access if macOS prompts. With both desktops unlocked, copy a formatted paragraph from a browser on Windows. Paste it into a Mac rich-text editor and then a plain-text editor. Repeat from Mac to Windows. Clipboard sharing works independently of which machine currently receives mouse and keyboard input. [Apple pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)

The limits are 64 KiB of UTF-8 plain text and 64 KiB of optional HTML per copy. The Windows adapter transfers the selected HTML fragment, so formatting that depends on styles outside the selection can be lost. The receiving editor controls which markup it imports.

To test images, use the browser's **Copy Image** command or copy a small screenshot to the clipboard. On Mac, use **Control+Shift+Command+4** and select a region. Paste into Paint on Windows or use **File → New from Clipboard** in Preview on Mac. The wire format is PNG, limited to 8 MiB, 4096 pixels per side, and 9,437,184 pixels total. Windows reads PNG and DIB and publishes PNG and DIBV5. macOS reads PNG, TIFF, and JPEG and publishes PNG and TIFF. On Mac, a supported raster image takes priority over captions and auxiliary pasteboard items. Telegram's **Copy Media** can supply both a raster image and a file reference. UniKVM uses the supplied image data, so it can paste into an image editor. On Windows, nonempty text takes priority.

Copying a JPEG file in Finder or Explorer uses a file reference and requires Phase 5 file transfer. To copy the image from a JPEG on Mac today, open it in Preview, select the image with **Command+A**, then copy it with **Command+C**.

The agent sends new copies made after the clipboard session starts and skips entries with supported sensitive-content markers. Remove the `[clipboard]` table and restart either agent to pause sharing.

## Watch clipboard progress

On either desktop, run this in a separate terminal while the agent is running.

```sh
cargo run --locked -p unikvm-cli -- clipboard
```

The command opens a native desktop window with separate rows for outgoing and incoming copies. The bar moves while a copy is pending. Quick copies may go straight to the result. **Ready to paste here** means the agent successfully wrote to this computer’s clipboard. The sender shows **Ready to paste on the other computer** after it receives confirmation. **This clipboard entry was skipped** can appear for copies such as Finder file references, unsupported formats, or privacy-marked items.

To check both directions, open the monitor on each machine, copy a large image from Preview or a Windows image editor, and wait for **Ready to paste here** before pasting. Repeat from the other machine. Close the window when you’re done. The agents continue sharing the clipboard.
