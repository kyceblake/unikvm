# Platform feasibility

Local status IPC was verified natively on macOS 27.3 (arm64). All 16 workspace tests passed at that milestone, including disconnect recovery and listener restart. Separate agent and CLI processes also passed status queries after abandoned client connections.

The Mac operator confirmed the standalone Quartz injection exercise from `7cb31ef` and the original Windows-to-Mac session on their desktop. Native macOS capture, permission loss, and bidirectional handoff remain pending.

Runtime verification was performed on Windows 11 Pro build 26300 in an ordinary desktop session. The other desktop rows summarize documented API paths. Availability depends on the installed desktop and granted session capabilities.

The Windows operator ran the physical suppression probe. It recorded two blocked F9 events, zero downstream F9 events, physical mouse movement, and successful Ctrl+Shift+F10 delivery.

| Desktop | Capture and suppression | Injection | Hotkeys and edge handling | Permissions and verification |
| --- | --- | --- | --- | --- |
| Windows | Low-level keyboard and mouse hooks on a message-pump thread. Verified physical F9 suppression and tagged synthetic event filtering. | `SendInput` with injection flags and extra-info marker verified for F24 press/release and relative mouse move. | Ctrl+Shift+F10 delivery verified in the physical probe. `SetCursorPos` reached two pixels at a monitor's right edge. Ordinary cross-monitor motion still needs testing. | Opening the input desktop and comparing it with the current thread desktop passed. UIPI blocks injection into higher-integrity apps. Secure desktops, protected targets, mixed DPI, and default Win-key bindings still need testing. |
| macOS | A Quartz session event tap filters synthesized events and suppresses local keyboard and mouse delivery while sending. Native capture verification is pending. | A Quartz adapter posts tagged keyboard, pointer, button, and scroll events. The operator confirmed the keyboard and pointer exercise. Mouse buttons and drag behavior still need desktop testing. | Ctrl+Shift+F9 and Ctrl+Shift+F10 use the tap. The receiving pointer is clamped to its starting display. | Grant Accessibility and Input Monitoring if required by the OS. Secure Input blocks or ends sharing. |
| Linux X11 | Evaluate XInput2 events and X11 keyboard/pointer grabs. Raw observation alone leaves normal event delivery in place. | Candidate XTEST. Verify whether the chosen capture path can distinguish generated input. | Candidate `XGrabKey`, RandR geometry and `XWarpPointer`. | Requires access to an X11 session and applicable extensions. Verify grab conflicts and release on client exit. Desktop tests pending. |
| GNOME Wayland | Candidate XDG InputCapture portal plus EIS with compositor-activated pointer barriers. | Candidate RemoteDesktop portal plus EIS. | The upstream backend lists GlobalShortcuts. The compositor controls activation and may ignore a requested cursor placement on release. | Requires detection of portal versions, consent, granted capabilities, and EIS devices. Desktop tests pending. |
| KDE Plasma Wayland | Candidate XDG InputCapture portal plus EIS. | Candidate RemoteDesktop portal plus EIS. | KDE lists GlobalShortcuts. Verify actual bindings, activation while captured, barriers, and release coordinates. | Requires detection of portal versions, consent, and capabilities. Desktop tests pending. |
| Sway/wlroots | The stock wlr portal lists Screenshot and ScreenCast. Evaluate compositor-specific layer-shell and pointer constraints only where exposed. | Candidate virtual pointer and virtual keyboard protocols where exposed by compositor policy. | Evaluate compositor bindings and supported layer-shell geometry for edge triggers. A generic cursor warp is unavailable through the stock wlr portal. | Desktop tests pending. Capabilities must be checked per compositor. |

On Windows, raw input may help with independent device data. Low-level hooks are the candidate suppression path, and their callbacks need to return promptly to avoid silent hook removal. Windows hotkey registrations can conflict or be reserved by the OS. [Keyboard hook](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc), [mouse hook](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc), [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput), [RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey), [SetCursorPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setcursorpos)

Wayland InputCapture distinguishes enabled from active. The documented barrier path leaves activation to the compositor. A granted global shortcut alone does not establish shortcut-triggered capture or guarantee emergency release. Probe those flows, zone changes, permission revocation, and compositor restart before enabling a backend. [InputCapture](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html), [RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html), [GlobalShortcuts](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html), [GNOME backend list](https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/blob/main/data/meson.build), [KDE backend list](https://github.com/KDE/xdg-desktop-portal-kde/blob/master/data/kde.portal), [wlr backend list](https://github.com/emersion/xdg-desktop-portal-wlr/blob/master/wlr.portal)

Other API references include [Apple event taps](https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate(tap:place:options:eventsofinterest:callback:userinfo:)), [Apple cursor warp](https://developer.apple.com/documentation/coregraphics/cgwarpmousecursorposition(_:)), [Xlib](https://www.x.org/releases/current/doc/libX11/libX11/libX11.html), and [XTEST](https://www.x.org/releases/current/doc/libXtst/xtestlib.pdf).

## Run the Windows probe

Run these from an interactive desktop with a harmless foreground window. The probe sends a paired F24 press/release and a one-unit relative pointer move, checks tagged hook delivery and suppression, and restores the starting pointer position. It collects counts without recording key identities.

```powershell
cargo run --locked -p unikvm-platform --example input_probe
cargo run --locked -p unikvm-platform --example input_probe -- --manual
cargo run --locked -p unikvm-platform --example input_probe -- --suppress-f9
```

For the manual run, move the mouse and press Ctrl+Shift+F10 during the ten-second window. For the suppression run, also tap F9 before the hotkey. A passing run reports at least two blocked physical F9 events and zero downstream F9 events. Only F9 is suppressed in this mode. Verify ordinary cross-monitor motion, permission changes, and elevated-window behavior on a dedicated test desktop before integrating a Windows backend.

## Run the macOS input probes

From Terminal in your logged-in Mac desktop session, use the first command to check permission. If access is denied, run the permission request and follow the macOS prompt or open **System Settings → Privacy & Security → Accessibility**. Repeat the check after granting access.

```sh
cargo run --locked -p unikvm-platform --example input_probe
cargo run --locked -p unikvm-platform --example input_probe -- --request-permission
```

Use the exercise command when you can focus a blank text editor during the five-second countdown.

```sh
cargo run --locked -p unikvm-platform --example input_probe -- --exercise
```

With a US layout, verify the editor shows `Ab` and the pointer moves. The probe also posts a scroll detent, which is easiest to see in a scrollable editor. Uppercase A followed by lowercase b checks that releasing a held synthetic Shift takes effect. Key positions follow the Mac's selected layout. The adapter attempts best-effort releases on cleanup. Permission loss and process termination still need native verification.

With the agent stopped, run the capture probe from the logged-in desktop. It counts physical event types while suppressing captured keys and clicks. Focus a blank editor and follow the terminal instructions. Hold Fn too if the top row defaults to media controls. To invoke the Input Monitoring prompt for the probe, use the second command.

```sh
cargo run --locked -p unikvm-platform --example input_probe -- --capture
cargo run --locked -p unikvm-platform --example input_probe -- --request-capture-permission
```

macOS may list the probe, terminal, and agent separately in privacy settings. Verify that the pointer stays in place during capture, local input resumes after Ctrl+Shift+F10, the editor received no typed text during capture, and the probe counted keys, movement, clicks, and scrolling. Quartz permits cursor disassociation in the foreground. The background fallback returns the pointer to its capture position after movement using a Quartz warp. Verify both behaviors on the target Mac. [Apple cursor control guide](https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/QuartzDisplayServicesConceptual/Articles/MouseCursor.html)

For remote reception, the Mac uses the active Quartz display mode. If it omits a fixed rate, the agent tries the nominal period reported by Core Video. ProMotion and variable-refresh modes can change between scanouts. Motion pacing uses the reported nominal period and falls back to immediate injection if macOS reports no usable period. [Quartz display mode refresh rate](https://developer.apple.com/documentation/coregraphics/cgdisplaymode/refreshrate), [Core Video nominal refresh period](https://developer.apple.com/documentation/corevideo/cvdisplaylinkgetnominaloutputvideorefreshperiod(_:))

Put the pointer on the display you want to test and run the refresh probe. It reports the rate used by the receiver.

```sh
cargo run --locked -p unikvm-platform --example input_probe -- --refresh
```

The probes run separately from the agents. On macOS, posting keyboard or pointer events requires the explicit exercise flag. [Apple posting permission](https://developer.apple.com/documentation/coregraphics/cgpreflightposteventaccess()), [Apple event posting](https://developer.apple.com/documentation/coregraphics/cgevent/post(tap:))
