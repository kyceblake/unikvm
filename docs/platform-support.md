# Platform feasibility

Local status IPC was verified natively on macOS 27.3 (arm64). All 16 workspace tests passed at that milestone, including disconnect recovery and listener restart. Separate agent and CLI processes also passed status queries after abandoned client connections.

The Mac operator confirmed the standalone Quartz injection exercise from `7cb31ef` and the original Windows-to-Mac session on their desktop. Native macOS capture, permission loss, and bidirectional handoff remain pending.

Runtime verification was performed on Windows 11 Pro build 26300 in an ordinary desktop session. The other desktop rows summarize documented API paths. Availability depends on the installed desktop and granted session capabilities.

The Windows operator ran the physical suppression probe. It recorded two blocked F9 events, zero downstream F9 events, physical mouse movement, and successful Ctrl+Shift+F10 delivery.

The native clipboard window showed the active clipboard session on Windows using the paired Mac. The AppKit window passed Apple Silicon cross-target Clippy. Its appearance and behavior still need a Mac desktop test. The native monitor is available on Windows and macOS. [Clipboard monitor instructions](clipboard-sync.md#watch-clipboard-progress)

For clipboard v5, a Windows test round-tripped multiple Unicode paths through the native `CF_HDROP` format. A desktop probe displayed a synthetic 6 MB offer and verified **Decline**. Apple Silicon strict Clippy passed for the AppKit prompt and pasteboard adapter. A real Explorer-to-Finder paste and the reverse direction still need testing with both agents updated.

For clipboard v6, the Windows native hook probe intercepted a UniKVM-tagged Ctrl+V for a pending file offer and passed the shortcut through when the clipboard sequence differed. Apple Silicon strict Clippy passed for the Quartz shortcut path. The live Windows-to-Mac and Mac-to-Windows paste flow still needs verification with both agents updated.

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

## Probe file drags at the screen edge

Run the standalone probe from an interactive Windows or Mac desktop.

```sh
cargo run --locked -p unikvm-platform --features file-drag-probe --example file_drag_probe -- --left
```

Use `--right` to test the other edge. Drag a regular file from Explorer or Finder into the highlighted strip and hold the mouse button. The probe prints the number of file references it can read before release. It declines the drop and closes after 45 seconds.

Live Explorer and Finder gestures both exposed file references before release. The Windows test also reads a real Shell data object. The probe uses OLE `IDropTarget` and AppKit `NSDraggingInfo`. [Windows Shell drag data](https://learn.microsoft.com/en-us/windows/win32/shell/dataobject) · [AppKit drag pasteboard](https://developer.apple.com/documentation/appkit/nsdragginginfo/draggingpasteboard)

The Windows source helper observes one file on the configured edge during an unmodified primary-button drag. The input monitor checks that the same physical press is still held on the unlocked desktop before ending the Explorer drag after a peer grant. The Windows source uses the standard left-button primary mapping. [Windows OLE drop target](https://learn.microsoft.com/en-us/windows/win32/api/oleidl/nf-oleidl-idroptarget-dragenter) · [Windows input injection](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)

The live source and receiver strips highlight only while the pointer is over them. Move the pointer away to check that the strip disappears. Then drag a single file across the edge immediately after entering the strip. The first press should still start the native receiver drag. The strips use a minimum nonzero opacity at rest to preserve native hit testing. [Win32 layered window hit testing](https://learn.microsoft.com/windows/win32/winmsg/window-features) · [AppKit window hit testing](https://developer.apple.com/documentation/appkit/nswindow/windownumber%28at%3Abelowwindowwithwindownumber%3A%29)

The Windows desktop regression verifies the tagged Escape press and release and suppresses both events before they reach the foreground application.

```powershell
cargo test --locked -p unikvm-platform explorer_drag_cancel_posts_a_tagged_escape_pair -- --ignored
```

### Probe a native drop into Explorer or Finder

Open an empty destination folder in Explorer or Finder on the desktop being tested. Keep it visible behind the terminal. Use a small disposable file outside that folder and run the probe with its path. Use `--right` on the Mac if the Mac is to the left of Windows.

```sh
cargo run --locked -p unikvm-platform --features file-drag-probe --example file_drag_probe -- --left --source "path/to/sample.txt"
```

Press and hold the left mouse button on the highlighted strip, drag into the empty folder, then release. The probe should print `Native drop target accepted a file copy.` Check that the new file matches the source and that the source remains in place. Repeat with Escape during the drag and check that the target folder stays empty. The probe uses a local file and supports one drag per run.

Repeat with the other computer's pointer after switching ordinary UniKVM control to the test desktop. That checks whether the native drag accepts UniKVM's injected mouse-button and movement events. The drag probe does not change the clipboard or transfer bytes between machines. Windows uses OLE `DoDragDrop` with a Shell file data object. macOS uses an AppKit dragging session with a file URL. [Windows DoDragDrop](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-dodragdrop) · [AppKit dragging session](https://developer.apple.com/documentation/appkit/nsview/begindraggingsession(with:event:source:))

### Probe a deferred Windows drop

Use the same test folder and a local sample file. The Windows probe advertises the filename and size as a Shell virtual file. Hold the drag over Explorer before you release and check that `Native target opened the file stream after release.` appears only when you drop. Check that the destination contains an identical copy. Start a fresh run with an empty destination and press Escape while holding the drag. The stream message should stay absent and the folder should stay empty. [Shell virtual file formats](https://learn.microsoft.com/en-us/windows/win32/shell/clipboard#cfstr_filecontents)

```powershell
cargo run --locked -p unikvm-platform --features file-drag-probe --example file_drag_probe -- --left --deferred "path/to/sample.txt"
cargo test --locked -p unikvm-platform --features file-drag-probe shell_folder_copies_the_virtual_file -- --ignored --nocapture
```

The second command exercises the native Shell folder drop handler directly and verifies the destination bytes. The first checks the full pointer gesture.

### Probe a deferred Finder drop

Open an empty Finder folder. Start the probe with a local sample outside that folder.

```sh
cargo run --locked -p unikvm-platform --features file-drag-probe --example file_drag_probe -- --right --deferred "path/to/sample.txt"
```

Press on the highlighted edge and hold over the Finder folder. The probe should print `Finder requested the promised file after release.` only when you drop. Compare the new file's SHA-256 with the source. Run again with an empty destination and press Escape while dragging. The folder should stay empty. This local probe exercises AppKit's file promise with an existing sample. [AppKit file promises](https://developer.apple.com/documentation/appkit/supporting-drag-and-drop-through-file-promises)

### Test Explorer with a paired file offer

Use the updated Windows agent and CLI in the same desktop session. Keep file sharing enabled for the paired sender. Copy one regular file on the sender, then open an empty Explorer folder on Windows. Run the command from the Windows workspace.

```powershell
cargo run --locked -p unikvm-cli -- drag-receive --left
```

Press and hold on the highlighted edge, move into the Explorer folder, and release. Windows should show the existing **Download / Decline** prompt only after release. Choose **Download** and confirm the file appears in the destination folder. Test **Decline** and Escape with fresh offers and empty folders. The receiving clipboard should keep its existing contents. The Windows tray reports **File dropped · Checksum verified** after Explorer reports a successful copy.

This preview begins with a clipboard offer from a paired peer. The command starts the native drag on Windows.

### Test a Finder drag across the edge into Explorer

Build the workspace and restart the agent and its desktop helper on both computers. In each local config, add `file_drag = true` under the existing `[input]` table. Keep bidirectional input, the configured peer side, and `[clipboard]` file sharing enabled for the same pinned peer. With the Mac on the left, use `peer_side = "right"` on the Mac and `peer_side = "left"` on Windows.

Open an empty Explorer folder on Windows. Drag one regular file in Finder with the Mac's mouse or trackpad. Keep holding the primary button at the configured edge until the pointer appears on Windows, then move into Explorer and release. **Download / Decline** should appear only after release. Approve the drop, compare the destination file's SHA-256 with the source, and confirm the original stays in place. The Windows clipboard should retain its previous contents.

Repeat with Escape before release and with **Decline** after release. Neither should create a destination file. Check that ordinary edge switching still works afterward. The preview supports one regular file per drag up to 50 MB. It requires an unlocked desktop and a free primary display edge. Restart the Mac helper after changing its display arrangement. [AppKit drag pasteboard](https://developer.apple.com/documentation/appkit/nsdragginginfo/draggingpasteboard) · [Windows OLE drag loop](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-dodragdrop)

### Test an Explorer drag across the edge into Finder

Update both agents and their desktop helpers. Use the same `input.file_drag = true`, peer sides, and pinned clipboard file-sharing settings as above. Open an empty Finder folder. With the Windows mouse, drag one regular file from Explorer to the left edge when the Mac is on the left. Keep holding until the pointer appears on the Mac, move into Finder, and release.

Approve **Receive** on the Mac and check that the receiving tray reports **File dropped · Checksum verified**. Confirm that the file appears in Finder, the source remains in Explorer, and the Mac clipboard keeps its previous contents. Repeat with Escape before release and **Decline** afterward. Each canceled test should leave the destination empty and allow normal pointer control to resume. Use an unlocked desktop and a free primary display edge. One regular file up to 50 MB is supported per drag.

### Return a Finder drag using the Windows mouse

Update and build both desktops, retaining `input.file_drag = true` and clipboard file sharing on both. Using the Windows mouse, enter the Mac without holding a button. Start dragging one regular file in Finder, hold the primary button through the Mac's right edge, then drop into an empty Explorer folder. Approve **Download** and compare the destination SHA-256 with the retained source. Repeat with Escape before release and Decline after release. Check normal pointer movement and primary clicks afterward, including cancellation during the edge handoff.

The Windows sender advertises return support with `EDGE/2`; an older Mac rejects that input request, so both agents must be updated. The return binds the file offer to the live peer connection and a fresh nonce. Mac keeps receiving input until Windows arms its native helper, then ends Finder's injected drag. Windows restores the physical primary-button hold on its capture thread and lets its real release reach OLE. File bytes remain gated by native release and receiver approval. The paired Windows-mouse gesture, Download prompt, and successful destination file opening have been confirmed. Native cancellation and a separate source/destination SHA-256 comparison remain to be checked.

## Run the Windows probe

Run these from an interactive desktop with a harmless foreground window. The probe sends a paired F24 press/release and a one-unit relative pointer move, checks tagged hook delivery and suppression, and restores the starting pointer position. It collects counts without recording key identities.

```powershell
cargo run --locked -p unikvm-platform --example input_probe
cargo run --locked -p unikvm-platform --example input_probe -- --manual
cargo run --locked -p unikvm-platform --example input_probe -- --suppress-f9
cargo test --locked -p unikvm-platform clipboard_offer_intercepts_tagged_paste_only_for_the_current_copy -- --ignored --nocapture
```

For the manual run, move the mouse and press Ctrl+Shift+F10 during the ten-second window. For the suppression run, also tap F9 before the hotkey. A passing run reports at least two blocked physical F9 events and zero downstream F9 events. Only F9 is suppressed in this mode. Verify ordinary cross-monitor motion, permission changes, and elevated-window behavior on a dedicated test desktop before integrating a Windows backend.

On the ordinary Windows desktop, the sender hides its parked pointer with a nearly transparent, non-activating window under the pointer. The capture worker owns that window and releases it on return. With the mouse stationary and no keys held, the native regression checks that capture installs the blank cursor, then restores a visible cursor at the left edge without changing the foreground window. Test fullscreen and elevated apps separately. [Microsoft cursor ownership](https://learn.microsoft.com/windows/win32/menurc/about-cursors)

```powershell
cargo test --locked -p unikvm-platform parked_cursor_hides_and_restores_without_activation -- --ignored --nocapture
```

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

macOS may list the probe, terminal, and agent separately in privacy settings. Verify that the pointer hides during capture, reappears at its anchor after Ctrl+Shift+F10, local movement resumes, the editor received no typed text during capture, and the probe counted keys, movement, clicks, and scrolling. Public Quartz cursor control normally requires a foreground app. The headless sender enables the private `SetsCursorInBackground` property on its own WindowServer connection, balances hide/show calls, and clears the property on cleanup. Missing symbols or ineffective hiding reject capture. Capture also retains its anchor and corrects drift on mouse events and monitor polls. The paired Mac-to-Windows desktop test confirmed hiding and restoration; other macOS versions and foreground changes need further verification. [Apple cursor control guide](https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/QuartzDisplayServicesConceptual/Articles/MouseCursor.html)

With the agent stopped, run the native cursor-drift regression explicitly. It verifies actual hiding, moves the pointer about 24 pixels, checks repeated corrections back to the capture anchor, and verifies restored visibility on release. It restores the starting position on cleanup. This passed on the target Apple Silicon Mac. It checks native parking and visibility; physical behavior still needs the capture probe or paired desktop test on each target machine.

```sh
cargo test --locked -p unikvm-platform cursor_parking_recovers_native_drift -- --ignored --nocapture
```

For remote reception, the Mac uses the active Quartz display mode. If it omits a fixed rate, the agent tries the nominal period reported by Core Video. ProMotion and variable-refresh modes can change between scanouts. Motion pacing uses the reported nominal period and falls back to immediate injection if macOS reports no usable period. [Quartz display mode refresh rate](https://developer.apple.com/documentation/coregraphics/cgdisplaymode/refreshrate), [Core Video nominal refresh period](https://developer.apple.com/documentation/corevideo/cvdisplaylinkgetnominaloutputvideorefreshperiod(_:))

Put the pointer on the display you want to test and run the refresh probe. It reports the rate used by the receiver.

```sh
cargo run --locked -p unikvm-platform --example input_probe -- --refresh
```

The probes run separately from the agents. On macOS, posting keyboard or pointer events requires the explicit exercise flag. [Apple posting permission](https://developer.apple.com/documentation/coregraphics/cgpreflightposteventaccess()), [Apple event posting](https://developer.apple.com/documentation/coregraphics/cgevent/post(tap:))
