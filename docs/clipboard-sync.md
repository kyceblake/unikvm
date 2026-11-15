# Share text, HTML, images, and files between Windows and Mac

Use a working [paired connection](manual-pairing.md) and a clipboard-enabled build on both machines. Add this table to each machine's `local-pairing/agent.toml`, using the other machine's pinned fingerprint. If you already use input sharing, copy the value from that machine's `[input]` table.

```toml
[clipboard]
peer_fingerprint = "OTHER_DEVICE_FINGERPRINT"
files = true
```

Build the workspace with `cargo build --locked --workspace` so the agent and native prompt helper are in the same directory. Restart both agents and wait for `clipboard sync connected`. Both agents need clipboard protocol version 5. File copying requires `files = true` on both peers. You can omit that option to share text, HTML, and images. On macOS, allow pasteboard access if macOS prompts. Clipboard sharing works independently of which machine currently receives mouse and keyboard input. [Apple pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)

Copies of up to **5 MB** transfer automatically. Copies over **5 MB** and up to **50 MB** show a native **Receive** or **Decline** prompt. The offer expires after 60 seconds. The size counts the whole copy, including multiple files or both plain text and HTML. MB here means 1,000,000 bytes. The Windows adapter transfers the selected HTML fragment, and the receiving editor controls which markup it imports.

To paste image pixels into an editor, use **Copy Image** or copy a screenshot. Paste into Paint on Windows or use **File → New from Clipboard** in Preview on Mac. UniKVM encodes images from apps as PNG, with limits of 4096 pixels per side and 9,437,184 pixels total. Windows reads PNG and DIB and publishes PNG and DIBV5. macOS reads PNG, TIFF, and JPEG and publishes PNG and TIFF. If an app supplies a raster image with an auxiliary file reference, UniKVM uses the image so it can paste into an image editor.

To copy the original file, select it in Explorer or Finder and copy it. When the receiver shows **Ready to paste here**, paste into an open folder or an app that accepts files. UniKVM stages the files locally and passes their paths to the receiving OS. The folder or app handles the paste. Up to 32 regular files can go in one copy. For example, a PNG copied as a file keeps its original bytes and bypasses the image pixel limits. Folder selections and app-specific virtual files are outside this release.

The agent sends new copies made after the clipboard session starts and skips entries with supported sensitive-content markers. Remove the `[clipboard]` table and restart either agent to pause sharing.

## Watch clipboard progress

On either desktop, run this in a separate terminal while the agent is running.

```sh
cargo run --locked -p unikvm-cli -- clipboard
```

The command opens a native desktop window with outgoing and incoming rows. It shows when a copy is waiting for approval and displays the number of bytes transferred. The approval prompt opens even with the monitor closed. **Ready to paste here** means the agent successfully wrote to this computer’s clipboard. The sender shows **Ready to paste on the other computer** after it receives confirmation.

Test both directions with a small file and a file over 5 MB. Decline a large offer and verify the receiving clipboard keeps its prior content. Repeat with **Receive**, wait for **Ready to paste here**, then paste into a folder and compare the file with the original.
