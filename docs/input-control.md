# Try Windows-to-Mac input control

Start from a working [paired connection](manual-pairing.md). Use the same unikvm revision on both machines. Stop the running agents before changing their configs. Keep the existing identity, peer certificate, and network settings.

Add this top-level table to the Windows `local-pairing/agent.toml`. Use the same Mac fingerprint that you already trust in the network config.

```toml
[input]
mode = "send"
peer_fingerprint = "MAC_FINGERPRINT"
```

On the Mac, add the following table using the Windows fingerprint. Grant Accessibility to the process running the Mac agent if System Settings requests it. The standalone probe and the agent executable may appear as different entries.

```toml
[input]
mode = "receive"
peer_fingerprint = "WINDOWS_FINGERPRINT"
```

Restart each agent from a terminal.

```sh
cargo run --locked -p unikvm-agent -- --config local-pairing/agent.toml
```

1. Wait for `authenticated peer connected` on both agents. Focus a blank editor on the Mac.
2. Press **Ctrl+Shift+F9** on Windows and release the keys. When the Mac grants the session, typing and pointer movement should go to the Mac.
3. Press **Ctrl+Shift+F10** on Windows to return locally. Try a few keys on both desktops to confirm Shift, Ctrl, and mouse buttons have released.
4. Start another session. On the Mac, stop the agent to verify Windows resumes local input. Restart the Mac agent and start a new session manually.
5. Test the opposite disconnect while holding a harmless key or mouse button. Stop the Windows agent and verify the Mac releases any synthetic input the receiver recorded.

Both sides opt in to one pinned fingerprint. The Windows hook filters synthesized Windows events and coalesces pointer movement. The Mac uses its own keyboard layout and confines the remote pointer to the display where each session begins.

The current implementation supports Windows sending and macOS receiving. It uses fixed shortcuts. Local activity on the Mac and hard termination of the Mac agent while it holds synthetic state need further work. The receiver attempts releases during normal shutdown, stream closure, and lease expiry. macOS can block event posting after Accessibility revocation, including cleanup releases. Use a blank editor for the first test, and keep both machines physically accessible.
