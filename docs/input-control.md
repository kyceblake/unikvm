# Share input between Windows and Mac

Start from a working [paired connection](manual-pairing.md). Use the same unikvm revision on both machines. Stop the running agents before changing their configs. Keep the existing identity, peer certificate, and network settings.

On **both machines**, edit the top-level input table in `local-pairing/agent.toml`.

```toml
[input]
mode = "bidirectional"
peer_fingerprint = "OTHER_DEVICE_FINGERPRINT"
```

Use the Mac's fingerprint on Windows and the Windows fingerprint on the Mac. You can also use `send` or `receive` to limit a machine to one direction. On macOS, grant **System Settings → Privacy & Security → Accessibility** to the executable or terminal macOS attributes the permission to. Grant **Input Monitoring** if macOS requests it. Restart the agent after changing permissions. For standalone probe commands, see [platform support](platform-support.md#run-the-macos-input-probes).

Restart each agent from a terminal on the logged-in desktop.

```sh
cargo run --locked -p unikvm-agent -- --config local-pairing/agent.toml
```

## Switch at the display edge

For a Mac immediately to the left of the PC, add `peer_side = "left"` to the Windows `[input]` table and `peer_side = "right"` to the Mac table. Both agents need bidirectional mode and a build with edge switching. Restart them after editing.

Move the PC mouse to the left edge of the primary Windows display. The pointer should appear near the Mac's right edge at the matching relative height. Move back through that edge with the same mouse to return. For the reverse test, move the Mac trackpad cursor to the Mac's right edge, then use that trackpad to cross back from the left edge of Windows. Release any held keys and buttons before crossing. Ctrl+Shift+F9 starts shortcut-based sharing and Ctrl+Shift+F10 ends either type of session.

The fixed layout uses the primary displays and reserves edges occupied by local monitors for the native desktop. Restart the agents after changing the primary displays. The sender hides its parked cursor while forwarding input. When you cross back, the sender's cursor reappears at the matching height and the receiver's cursor hides until local input or the next handoff. On Windows, edge sharing reads relative motion from Raw Input while the cursor is parked. Try several quick crossings in each direction, then use one blank editor on each machine to verify that keyboard focus follows the pointer. Monitor selection and synchronized layouts remain future work.

## Test input sharing

1. Wait for `authenticated peer connected` on both agents. Focus a blank editor on the Mac.
2. Press **Ctrl+Shift+F9** on Windows and release the chord. Type a few characters and move, click, and scroll the pointer. The Mac should receive the input. On Mac keyboards with media actions on the top row, hold **Fn** as well to send F9 or F10.
3. Use the **Mac's physical keyboard or trackpad** while Windows is sending. A key, click, scroll, or several pixels of movement gives the Mac back local control and ends forwarding from Windows. Try typing in the Mac editor while Windows moves its mouse. Both devices should now respond to their own hardware.
4. Focus a blank editor on Windows and use **Ctrl+Shift+F9** on the Mac to start in the other direction. Repeat the keyboard, click, scroll, and local takeover checks. **Ctrl+Shift+F10** on the sender also ends a session.
5. Repeat takeover with a harmless remote key or mouse button held. Verify the receiving agent releases its synthetic state. Try starting both directions together and check that at most one is granted. Stop the sender during a session, then the receiver during a new session, and check that local input recovers. Starting after reconnect requires a fresh shortcut.

While either machine sends input, check that its local cursor hides. End sharing with Ctrl+Shift+F10 and check that the cursor reappears at the capture position and moves normally. Test Windows-to-Mac motion at 60 Hz and at a higher refresh rate if your Mac supports one. Windows paces motion using the monitor under the pointer. macOS paces motion using the display where the session starts. Each receiver can combine faster samples between updates or spread slower samples across several display intervals. These updates use the rate reported by the OS and are not synchronized to individual scanouts. When no rate is available, the receiver applies motion as it arrives.

Each endpoint grants one input session at a time. If both endpoints already have outgoing requests, both requests can be denied. Press the start shortcut again on the device you want to send from. Using physical input on the receiver restores local control automatically.

Both backends filter their own synthetic events. The receiver attempts to release held synthetic input on takeover, normal shutdown, stream closure, or lease expiry. The OS may prevent cleanup after permission loss, on protected desktops, or when the receiver is force-killed. The first local event can arrive before asynchronous synthetic releases finish. Test modifier and drag behavior on both desktops before relying on it.

Windows injection runs on the ordinary input desktop and is subject to application integrity levels. macOS uses the selected keyboard layout and confines its synthetic pointer to the display where a receive session begins. Secure Input can stop a session. Mac capture forwards ordinary trackpad movement, clicks, and scrolling. Multitouch gestures and media keys are outside this milestone.
