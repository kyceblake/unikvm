# Share text, HTML, images, and files between Windows and Mac

Use a working [paired connection](manual-pairing.md) and a clipboard-enabled build on both machines. Add this table to each machine's `local-pairing/agent.toml`, using the other machine's pinned fingerprint. If you already use input sharing, copy the value from that machine's `[input]` table.

```toml
[clipboard]
peer_fingerprint = "OTHER_DEVICE_FINGERPRINT"
files = true
```

To share with several devices, set `peer_fingerprints = ["PEER_A_FINGERPRINT", "PEER_B_FINGERPRINT"]` in place of the singular setting. Each fingerprint must already be pinned under `[[network.peers]]`, and each recipient must opt in to clipboard sharing with the sender. Input sharing retains its own peer setting.

Build the workspace with `cargo build --locked --workspace` so the agent and native prompt helper are in the same directory. Restart the agents and wait for `clipboard sync connected`. All clipboard peers need protocol v6. File copying requires `files = true` on both ends of a connection. You can omit that option to share text, HTML, and images. On macOS, allow pasteboard access if prompted and grant the agent the [keyboard capture permissions](input-control.md) to use ⌘V for incoming file offers. [Apple pasteboard access](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)

Text, HTML, and app image copies up to **5 MB** transfer automatically. Larger inline copies up to **50 MB** show a native **Receive** or **Decline** prompt. Every file copy through **50 MB** requires approval after Paste, regardless of size. The size counts the whole copy, including multiple files or both plain text and HTML. MB here means 1,000,000 bytes. The Windows adapter transfers the selected HTML fragment, and the receiving editor controls which markup it imports.

To paste image pixels into an editor, use **Copy Image** or copy a screenshot. Paste into Paint on Windows or use **File → New from Clipboard** in Preview on Mac. UniKVM encodes images from apps as PNG, with limits of 4096 pixels per side and 9,437,184 pixels total. Windows reads PNG and DIB and publishes PNG and DIBV5. macOS reads PNG, TIFF, and JPEG and publishes PNG and TIFF. If an app supplies a raster image with an auxiliary file reference, UniKVM uses the image so it can paste into an image editor.

To copy the original file, select it in Explorer or Finder and copy it. On the receiving device, use **Ctrl+V** on Windows or **⌘V** on macOS to open the confirmation, then select **Receive**. Only that peer starts downloading the file. Each other peer can request and approve its own transfer. Once the tray shows **Checksum verified · Ready to paste**, use Paste again in an open folder or app that accepts files. UniKVM checks the BLAKE3 digest against the sender's before placing references to the staged files on the clipboard. The folder or app handles the final placement. A new local clipboard copy cancels the pending or in-flight incoming file. Unrequested file offers expire after 10 minutes, and approval prompts expire after 60 seconds. Up to 32 regular files can go in one copy. A PNG copied as a file keeps its original bytes and bypasses the image pixel limits. Folder selections and app-specific virtual files are outside this release.

The agent sends new copies made after the clipboard session starts and skips entries with supported sensitive-content markers. Remove the `[clipboard]` table and restart either agent to pause sharing.

<a id="watch-clipboard-progress"></a>

## Use the tray and menu bar

The agent opens a Windows tray icon or macOS menu bar icon when the `unikvm` executable is beside it. Hover to see paired computers and clipboard status. Open the icon menu for the full device list, **Connect**, **Disconnect**, and **Exit completely**. Disconnect pauses this computer's shared input and clipboard until you connect again. Add an optional `name = "MacBook"` to each `[[network.peers]]` entry to give it a local display name.

If you need to open the icon manually, run:

```sh
cargo run --locked -p unikvm-cli -- tray
```

Windows asks **Download** or **Decline** in a native dialog. macOS asks **Receive** or **Decline**. When approved, Windows uses the system file progress dialog and macOS shows a small progress panel near the top right of the screen. **Cancel** stops the transfer and clears partial data. The receiver's tray shows **Checksum verified · Ready to paste** once the clipboard is updated. The sender's tray shows **Checksum verified by** the peer after the receiver confirms success.

Test both directions with a small file and a file over 5 MB. Copy a file and verify the confirmation opens only after Ctrl+V or ⌘V on the receiving computer. Decline and verify the receiving clipboard keeps its prior content. Repeat with **Download** or **Receive**, wait for **Checksum verified · Ready to paste**, and paste into a folder. With multiple peers, approve on one device and verify the others stay at zero bytes. Repeat after making a newer local copy on the receiver and verify normal Paste uses that copy.
