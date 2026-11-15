# Progress

## Performance baseline and optimization

- Added a 1 ms scheduling deadline for the first useful step of new mouse motion on both receivers. Remaining motion keeps its display cadence, and pending movement still posts before keys and clicks.
- macOS now shares one access check between edge detection and pointer posting. Its diagnostics split access validation, event preparation, and the CoreGraphics post call. Acknowledgment summaries include Quinn path RTT and loss. The diagnostic budget is now 3 ms.
- Windows formatting, strict Clippy, 84 workspace tests, and debug and release builds passed. The native Windows pointer probe passed on the 240 Hz display. Apple Silicon passed strict Clippy at compile time. The paired latency target and cursor feel still need native verification.
- The first Mac → Windows run on a 240 Hz receiver measured 1.68 ms mean and 21.53 ms maximum from received motion to native posting across 696 samples. Windows checksum-verified a 2.54 MB file at 2.10 MB/s. The slowest motion sample occurred before that transfer.
- Extended diagnostics with a 5 ms budget and 95th percentile by input category. Each summary breaks down the same worst sample so sender-side submission, round-trip acknowledgment, receiver pacing, and native posting can be compared.
- Windows formatting, strict Clippy, 82 workspace tests, and debug and release builds passed. Apple Silicon passed strict Clippy at compile time. The extended fields still need a paired run.
- The operator confirmed the paired file-sharing checks passed. Phase 6 now defines a performance baseline before tuning and release hardening.
- Added opt-in `diagnostics = true` for the regular agent, including release builds. It logs sampled input acknowledgment round trips, receiver native posting times, and sender and verified receiver file throughput. The existing peer protocol and input pacing remain compatible. [Measurement definitions and baseline procedure](performance.md)
- Windows formatting, strict Clippy, 80 workspace tests, and debug and release builds passed. The Apple Silicon workspace passed strict Clippy at compile time.
- Next, record Windows ↔ macOS latency and verified receiver throughput with release builds, at idle and during a 50 MB transfer. Compare p95 against 3 ms and check cursor smoothness, edge handoff, and file drags.

## File verification and drag edge visibility

- Both trays show the result of the existing BLAKE3 verification for received files. The receiver reports a match after publishing the clipboard or completing the native drop. The sender reports a match after the peer's success receipt. [Clipboard file verification](clipboard-sync.md#use-the-tray-and-menu-bar)
- Windows and macOS edge strips show the blue highlight only under the pointer. At rest, the strip keeps the native hit area at minimum opacity so the arriving drag can start immediately.
- Windows formatting, strict Clippy, all 78 workspace tests, and build passed. The Windows desktop probe confirmed idle hit testing, hover, and clearing the highlight on exit. The Apple Silicon workspace and drag probe passed strict Clippy at compile time.
- Next, verify the checksum status, hover, and immediate drag handoff on the paired desktops.

## Explorer-to-Finder held-button handoff

- Native testing found a post-download deadlock: AppKit's file-promise provider already held a coordinated destination write, and the delegate opened a second coordinator for the same URL. Removed the nested claim while retaining atomic no-clobber publication. A native regression writes under an existing coordination claim and passed; workspace tests, formatting, strict Clippy, and build passed. Added content-free native delivery error diagnostics and stopped the stalled helper processes. The operator confirmed successful Windows-to-Finder file delivery after the fix. Also delayed native readiness until the AppKit event loop is running and its window is displayed; the first injected press then starts the drag without requiring a second blue-strip gesture. The operator confirmed the continuous drag works. Independent destination SHA-256 and cancellation checks remain pending.
- Pulled `7d91b6b` and added the native Finder receiver for the existing Explorer drag source. The Mac helper arms an AppKit file promise before Windows hands off the held pointer. Finder requests the file after release, then macOS shows **Receive / Decline**. The helper writes the verified copy to the native destination and leaves the clipboard intact. The existing peer protocol and pairing settings still apply.
- Windows formatting, strict workspace Clippy, 77 tests, and workspace build passed. Updated the optional Windows probe's test fixtures for the current drag source fields. All four Windows drag tests passed, including the native Shell destination test. Cargo deny passed with existing duplicate-version warnings. The Apple Silicon workspace and deferred drop probe passed strict Clippy as compile-time checks.
- Next, run the local [deferred Finder probe](platform-support.md#probe-a-deferred-finder-drop) and the [paired Explorer-to-Finder test](platform-support.md#test-an-explorer-drag-across-the-edge-into-finder) on macOS. Verify matching SHA-256 hashes, source retention, Escape, Decline, and normal pointer control after the drop.

## Windows mouse returning a Finder drag

- The paired retry exposed Quartz reporting the injected primary hold through hardware-state queries. Removed that ambiguous query from Mac remote-drag admission/cancellation; the sink's authenticated held state and the session's physical-takeover monitor remain authoritative. The operator confirmed that Windows-owned dragging now crosses back to Windows and opens Download/Decline. The operator approved Download and confirmed the file arrived in Explorer and opened correctly; a separate source/destination SHA-256 comparison remains unverified. Mac formatting, strict Clippy, workspace tests, and build passed.
- Added a separate return handoff for a Windows mouse controlling Finder on the Mac. It uses the existing receiving input session and a nonce-bound file offer, rather than treating injected events as physical Mac input. Windows arms its helper before Mac cancels Finder's drag, then restores local pointer ownership and the suppressed primary hold on its hook thread. The native release and approval still gate file delivery.
- Added protocol, nonce/stage cancellation, pending-offer cleanup, and local activation/release regressions. Mac workspace tests, build, and strict Clippy passed. Windows platform and exact CLI drop-helper module passed cross-target strict Clippy; a full Windows CLI cross-build on this Mac is blocked by missing Windows SDK C headers for ring. The operator subsequently confirmed the paired Windows-mouse drag, approval, and successful file opening as recorded above. Native cancellation tests for this return path remain pending. See the [return-drag test](platform-support.md#return-a-finder-drag-using-the-windows-mouse).

## Explorer as a native drag source

- The Windows desktop helper observes a single Explorer file at the configured display edge. The agent leaves the drag in Explorer until a pinned peer grants the handoff. It then ends the local drag and captures pointer movement and the physical button release. [Windows source checks](platform-support.md#probe-file-drags-at-the-screen-edge)
- The Windows Shell test confirmed that the observer reads the file reference during a primary-button drag and declines the local drop. The desktop hook test verified the tagged Escape pair. Windows formatting, workspace Clippy, build, and 74 tests passed. The Apple Silicon workspace passed strict Clippy at compile time.
- Next, add Finder's deferred file-promise receiver and verify the Windows-to-Mac gesture on the paired desktops.

## Finder-to-Explorer held-button handoff

- A native trackpad retry exposed right-edge rounding: all input eligibility checks passed, but the cursor rounded to x=1280 on a 1280-wide display and was rejected as outside. Native position conversion now floors fractional coordinates into their containing pixel. The regression failed before the fix and passed afterward; formatting, strict workspace Clippy, workspace tests, and build passed. The operator confirmed that the fixed trackpad drag crosses to Windows and opens the Download/Decline prompt. Final destination bytes and cancellation still need verification for this cross-machine gesture.
- Added the opt-in `input.file_drag` preview. Finder's edge helper supplies one file. The paired Windows agent opens the existing Explorer drop helper before granting pointer control, then allows a file request only after the forwarded button release. The receiver still approves the transfer and the selected folder receives verified bytes. [Desktop test instructions](platform-support.md#test-a-finder-drag-across-the-edge-into-explorer)
- Added checks for file-sharing configuration, metadata-only drag offers, session and connection binding, and cancellation before release. Windows formatting, workspace Clippy, build, and 73 tests passed. The Windows native Shell drop test and Cargo deny passed. The Apple Silicon workspace passed strict Clippy as a compile-time check.
- Next, run the paired Mac-to-Windows desktop test to verify the native handoff, cancellation, and ordinary input afterward. The reverse direction still needs native Finder delivery.

## Explorer delivery of a paired file offer

- Connected the Windows native Shell drop to the existing approval, progress, and verified file transfer. The `drag-receive` preview uses a pending single-file offer. Releasing over Explorer requests it, and the receiving clipboard keeps its existing contents. The peer clipboard protocol stays at v6. [Preview instructions](platform-support.md#test-explorer-with-a-paired-file-offer)
- Windows workspace formatting, strict Clippy, tests, and build passed. The native Shell test confirmed that the destination requests the verified file only after release and copies identical bytes. The new IPC command passed an isolated same-user named-pipe test. The Apple Silicon platform check passed at compile time. Cargo deny passed.
- The user confirmed the live Mac-to-Windows Explorer drop with the updated Windows agent and a paired Mac.

## Deferred Windows drop probe

- Added `--deferred FILE` on Windows. The probe offers a Unicode filename and size while the drag is held. It opens a read-only stream when the released drag's target requests contents. Escape leaves the stream closed. [Windows test instructions](platform-support.md#probe-a-deferred-windows-drop)
- The Windows COM tests verified metadata, cancellation, the release gate, and original bytes. The native Shell folder handler copied identical bytes into a temporary destination. The user reported that the interactive Explorer checks passed, and the dropped file's SHA-256 matched the source. Windows workspace checks and strict Clippy for the probe passed. Apple Silicon probe Clippy passed as a compile-time check.
- Next, probe Finder's deferred delivery with AppKit file promises, then bind native drop requests to receiver approval on the paired connection. [AppKit file promises](https://developer.apple.com/documentation/appkit/nsfilepromiseprovider)

## Native destination drop probe

- Native Apple Silicon source probe built successfully. Both the Mac mouse and Windows mouse controlling the Mac dragged the right-edge sample into Finder; AppKit reported accepted copies. Both destination SHA-256 hashes matched the retained source. Escape ended a further drag without an accepted copy; the cancellation destination stayed empty and the source remained intact. These are local native drops, not cross-machine file drags.
- Added `--source FILE` to start a copy-only native drag from the highlighted edge with a local sample file. The target app handles choosing the destination folder. [Drop probe instructions](platform-support.md#probe-a-native-drop-into-explorer-or-finder)
- Windows formatting, workspace Clippy, tests, and build passed. The standalone Windows probe built and passed its Shell file-reference test. The user reported that the sample appeared in Explorer. The temporary destination was empty at follow-up, so the dropped file's bytes remain unverified. Apple Silicon strict Clippy passed as a compile-time check. The Finder destination drop and Windows-controlled pointer checks passed as recorded above. The Mac-controlled pointer check on Windows remains pending.

## 2026-11-15 — File drag edge probe

- Added a standalone native probe that checks whether Explorer and Finder supply file references while the mouse button is held at a display edge. It reports counts and leaves the source files untouched.
- Windows passed the Shell data-object test. The Apple Silicon probe passed compile-time Clippy. Manual Windows and macOS tests confirmed that Explorer and Finder expose the file before the mouse button is released. [File drag probe instructions](platform-support.md#probe-file-drags-at-the-screen-edge)

## 2026-11-15 — Native tray and transfer dialogs

- Replaced the standalone clipboard monitor with the Windows tray icon and macOS menu bar item. The tray shows paired computers and transfer status. Connect, Disconnect, and Exit act on the local agent.
- File approval uses a Windows Task Dialog or macOS alert. The Windows Shell dialog and macOS progress panel show actual file byte counts. Cancel targets the current transfer through same-user IPC. The existing clipboard peer protocol and two-paste flow are compatible.
- Windows workspace checks and 69 tests passed, along with the native Windows Shell progress probe. Checked the Windows tray startup, single instance, Connect, Disconnect, and Exit over the live agent. Removed and restored the icon with a targeted `TaskbarCreated` message. Apple Silicon and Linux strict Clippy passed as compile-time checks. The dependency audit passed.
- Next, test the macOS menu bar, native approval, byte progress, cancel, and completed paste with both agents updated.

## 2026-11-14 — Phase 5 paste-triggered file transfers

- Clipboard v6 offers file names and sizes to explicitly configured peers. A receiving device requests an offer with Ctrl+V or ⌘V and confirms it in a native prompt. Only the approving peer receives bytes. A newer local clipboard copy takes priority. Once the transfer is ready, Paste places the verified files in the selected app or folder.
- Windows formatting, strict Clippy, build, and 65 workspace tests passed. The Windows desktop input probe and a dedicated tagged Ctrl+V regression passed. Apple Silicon and Linux strict Clippy passed as compile-time checks.
- Copying a file exposed a local status compatibility issue in the older clipboard monitor. The agent now reports the pending offer as a waiting state to older monitors and provides the full status to the current monitor.
- Next, update the paired Windows and Mac desktops and verify the native prompt, approval, and final paste in both directions. When a third desktop is available, verify that its clipboard and incoming byte count stay unchanged when only another device approves. [Setup and test steps](clipboard-sync.md)

## 2026-11-14 — Clipboard v5 file transfer

- Clipboard copies up to 5 MB transfer automatically. Larger copies up to 50 MB require a native receiver prompt. Files have a separate opt-in. Completed files are staged privately and published to the native clipboard for pasting into an app or folder.
- The Windows workspace build, strict Clippy and 62 standard tests passed. Tests covered a 50 MB batch, duplicate filenames, corrupt and truncated transfers, approval IPC, and simultaneous clipboard and input streams on authenticated loopback connections. Verified the Windows dialog’s Decline action. Apple Silicon and Linux cross-target Clippy and Cargo deny passed.
- Next, update both desktops and verify the native paste in each direction, the automatic prompt with the monitor closed, and mouse and keyboard responsiveness during a 50 MB transfer. [Setup and test steps](clipboard-sync.md)

## 2026-11-14 — Native clipboard monitor

- Replaced the browser monitor with a native Win32 window and an AppKit window. Run `unikvm clipboard` in the same desktop session as the agent. The CLI reads existing clipboard v4 status through local IPC.
- Windows formatting, strict Clippy, workspace build, and all 61 standard tests passed. The native window displayed the paired Mac connection through the running Windows agent. Apple Silicon and Linux cross-target Clippy passed, along with Cargo deny. The Apple check also caught and verified a fix for the existing macOS pasteboard array iteration.
- Next, open the monitor on the Mac and verify animation and paste readiness in both directions with a large image. [Clipboard monitor instructions](clipboard-sync.md#watch-clipboard-progress)

## 2026-11-14 — Clipboard progress monitor

- Added `unikvm clipboard` for Windows and macOS. The local browser panel shows incoming and outgoing activity, skipped entries, and confirmation from the destination. Clipboard protocol v4 adds a receipt tied to each update. The receiver sends success after writing to the native clipboard.
- Windows formatting, strict Clippy, build, and 61 workspace tests passed, including the native IPC exchange and bidirectional QUIC receipt exchange. Checked panel layout and state transitions in Edge. Linux cross-target Clippy and Cargo deny passed. Windows denied access when launching the Apple Silicon compiler for dependencies.
- Next, update both agents and use the [clipboard monitor instructions](clipboard-sync.md#watch-clipboard-progress) to repeat the full-size Preview copy and verify confirmations in both directions.

## 2026-11-14 — Telegram Copy Media compatibility

- Telegram for macOS puts an image item alongside a file reference. The Mac adapter now checks for explicit image data before excluding file-reference copies. It never opens the referenced file and still skips pasteboard privacy markers. [Telegram's Copy Media implementation](https://github.com/TelegramMessenger/telegram-macos/blob/246e3e957ae5cea1ecff6d7a6a6db3e47be69bb5/packages/ChatUI/Sources/ChatUI/ChatMessageMenuItems.swift#L312-L317)
- Added private pasteboard regressions for a file reference with an image item and a standalone Finder-style file reference. On the paired desktops, a Telegram photo and a fresh Mac screenshot both pasted into Paint. Windows logged both as received and applied. Copying a 3 MB JPEG in Finder left the previous Windows clipboard image in place, as expected before Phase 5 file transfer.
- Windows formatting, strict Clippy, workspace build, and all 58 standard tests passed. Windows denied access when starting the Apple Silicon cross-check of two dependencies, so the Mac regressions still need a native run.
- An initial full-image copy from Preview did not reach Windows. A small rectangular selection pasted into Paint. A subsequent retry left a synced PNG on the Windows clipboard matching the original photo's 1056 × 1280 dimensions, at about 1.2 MB. That transfer fits the image limits.

## 2026-11-14 — Mac image copy compatibility

- The operator confirmed Windows-to-Mac image paste. Windows logged outbound images and an active clipboard v3 session, with no incoming image during the Mac-to-Windows attempts.
- The Mac adapter now looks for supported raster data across a bounded number of pasteboard items, even when an image has accompanying text or browser metadata. It also reads JPEG representations. File copies and privacy markers remain excluded.
- Native Apple Silicon build exposed an unavailable array iterator with the selected Foundation features. Switched to bounded indexed access and removed an unused import. Formatting, strict workspace Clippy, workspace tests/build, and both private Mac pasteboard regressions passed locally.
- Windows formatting, strict Clippy, all 58 workspace tests, and the build passed on rerun after a transient three-peer test failure. Cargo deny passed. The Apple Silicon cross-check is still blocked by Windows access-denied errors starting the compiler.
- After updating the Mac agent, the operator confirmed that both a Mac screenshot and Discord's Copy Image command pasted into Paint. Windows logged both images as received and applied. Discord image copy from Windows to Mac also worked.
- Copying a JPEG file in Finder or Explorer requires Phase 5 file transfer. Open the JPEG in an app and copy the image itself to use clipboard image sync.

## 2026-11-14 — Phase 4 image clipboard sync

- Added a bounded PNG update in clipboard protocol v3. Windows converts PNG and DIB and publishes PNG with DIBV5. macOS converts PNG and TIFF and publishes both formats. Text and HTML continue to share one revision, and nonempty text takes priority on copies with both text and image data.
- Windows formatting, strict Clippy, build, 58 workspace tests, and the native clipboard regression passed. Tests covered PNG bounds, alpha and orientation, and authenticated bidirectional transport. Linux cross-target Clippy and cargo deny passed. Windows returned access denied while starting the compiler for Apple Silicon dependencies.
- Next, update both agents and test image copy and paste in both directions, including a screenshot and a transparent PNG. Repeat a formatted-text paste to check the existing path with clipboard protocol v3. [Clipboard test instructions](clipboard-sync.md)

## 2026-11-14 — Phase 4 HTML clipboard sync

- Added optional HTML alongside the plain-text representation under the same revision. Both representations are limited to 64 KiB each. Both agents need clipboard protocol version 2.
- Windows publishes HTML Format with UTF-8 byte offsets and transfers the selected fragment. macOS reads and writes HTML and plain text on the same pasteboard item. Both retain privacy markers and echo suppression. [Microsoft HTML Clipboard Format](https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format), [Apple pasteboard items](https://developer.apple.com/documentation/appkit/nspasteboarditem)
- Windows formatting, strict Clippy, build, 56 workspace tests, and the native clipboard regression passed. Linux cross-target Clippy passed. Windows returned access denied while starting the compiler for the Apple Silicon AppKit dependencies.
- The operator confirmed the browser clipboard test worked between the paired Windows and Mac desktops. The Windows log confirmed the clipboard v2 session connected.
- Next, add image clipboard sync.

## 2026-11-14 — Phase 4 native clipboard sync

- Added a separate `[clipboard]` opt-in for one pinned peer on Windows and macOS. A versioned readiness exchange lets two enabled agents share new text without sending whatever was on the clipboard at startup.
- Native adapters track clipboard ownership changes, skip supported sensitive-content markers, and mark imported text to avoid echo. The clipboard worker stays independent of keyboard and mouse handoff. See the [clipboard setup and paired test](clipboard-sync.md).
- Windows formatting, strict Clippy, build, and 53 workspace tests passed, including a bidirectional transfer near the 64 KiB limit over an authenticated loopback connection. The native Windows regression verified Unicode clipboard writes, suppression of received text, and rejection of a stale write. Linux cross-target Clippy and Cargo deny passed. The Apple Silicon cross-check was blocked by a Windows access-denied error when compiling the new AppKit dependencies.
- The operator confirmed live plain-text copy and paste in both directions between Windows and macOS. The Windows log also confirmed that clipboard sessions re-established after peer reconnects.
- Next, verify repeated copy/paste after reconnect, simultaneous copies, and the one-sided opt-in case on the paired desktops before expanding clipboard formats.

## 2026-11-14 — Phase 4 plain-text clipboard foundation

- Kept the fixed two-display handoff for the first release. Moved monitor enumeration, layout synchronization, and the layout editor to Phase 7 after release. Clipboard, file transfers, and release hardening are now Phases 4 through 6.
- Added a versioned plain-text update frame with a 64 KiB UTF-8 limit, authenticated-origin checks in the clipboard state, and per-session deduplication. Logical revisions break simultaneous-write ties by device ID.
- Windows formatting, strict Clippy, workspace build, and 51 workspace tests passed. Apple Silicon and x86-64 Linux passed cross-target Clippy. Cargo deny passed with existing duplicate-version warnings.
- Next, connect this format to a mutually opted-in clipboard stream and native Windows and macOS adapters. Baseline the current clipboard on connection without broadcasting it, skip entries marked sensitive by their source, and filter local notifications triggered by remote writes. Verify with bidirectional copy/paste on the paired desktops.

## 2026-11-15 — Opt-in locked Mac receiver and Windows feasibility probe

- Added `input.allow_locked_receive` (default false) for a pinned receiving peer. Mac reception requires this user's logged-in console session; Secure Input is allowed only while that eligible session is normally locked. Protected local input is never forwarded. The monitor survives suspension and recreates its complete event tap after unlock.
- The operator unlocked the Mac using Windows input and confirmed sharing resumed. Fixed a subsequent edge-return regression: the receiver retains its configured return side after unlock, and shortcut sessions close compatibly without sending an unexpected edge frame. The operator confirmed mouse crossing returns to Windows again.
- Added a capability-only Windows LocalSystem service/console-worker probe with a SYSTEM-only, PID-verified pipe and bounded reports. It never injects input. A protected-path administrative script installs, runs, reports, and removes the one-shot probe. Actual Windows locked receiving remains explicitly unsupported until native proof and a separately authenticated injector are implemented.
- Apple Silicon formatting, strict Clippy, workspace build and 46 standard tests passed. Windows platform/example cross-target strict Clippy passed; this is compile-time verification only. Read-only Mac session/permission probing passed. Native Windows installation and secure-desktop attachment remain pending operator testing.

## 2026-11-15 — Complete fast crossing before edge return

- Fixed a discarded crossing step: the receiver could report an edge return before applying the movement that reached it, leaving the visible cursor far inside the display. Both native sinks now place the cursor at the exact exit edge and interpolated crossing height before returning the handoff result. The Mac also updates its cached pointer and posts the tagged position.
- Schedule receiver hiding before the return write, so a stalled write does not delay the hide request; restore visibility if that write fails. The native monitor still applies visibility asynchronously, so strict cross-machine scanout ordering is not guaranteed.
- Apple Silicon formatting, strict Clippy, all 40 standard workspace tests, and workspace build passed. A native regression failed before the fix with the cursor 639 pixels inside the display, then passed for both horizontal edges with diagonal overshoot. The native receiver hide/restore test also passed. Windows runtime verification remains pending.
- Restarted the paired Mac client. The operator's initial fast Windows-mouse round-trip retest looked fixed; repeated physical crossings remain useful to confirm visual behavior under different speeds.

## 2026-11-14 — Hide the receiver's cursor on edge return

- The operator reported that when the Windows mouse crossed back to Windows, the Mac continued to show its cursor at the edge.
- A successful edge return now parks and hides the receiving cursor. Physical input, the next incoming handoff, or peer disconnection restores it. The native monitor also releases the hide when it stops or loses access to the desktop.
- The Windows workspace checks, six native desktop tests, and input probe passed. Apple Silicon and x86-64 Linux passed cross-target Clippy. The macOS visibility regression needs a logged-in Mac with Accessibility access.
- Next, repeat the Windows-mouse round trip on the paired desktops and touch the Mac trackpad after returning to confirm the Mac pointer reappears.

## 2026-11-14 — Lower-latency edge handoff and cursor visibility

- Send the first heartbeat with the edge request to remove a separate network round trip. Capture still starts after a matching grant and fresh acknowledgment. Initialize display pacing once the cursor is on the receiving display.
- Wake the native capture worker directly when control changes. A mapped return rearms the shared edge immediately. The sender still reads the return position if QUIC stops its write side first.
- Windows hides its parked cursor with a scoped, non-activating window. macOS continues to scope hiding to capture. The Windows desktop regression verified a blank cursor while forwarding, a visible cursor at the mapped return position, and unchanged foreground focus.
- Windows formatting, strict Clippy, workspace build, 45 workspace tests, five native desktop tests, and the input probe passed. Apple Silicon and x86-64 Linux passed cross-target strict Clippy.
- Next, verify fast back-and-forth crossings on the paired Windows and Mac desktops and check that only the active pointer remains visible during forwarding.

## 2026-11-14 — Edge handoff cursor placement

- The first paired test confirmed edge sessions and return messages. The operator reported a visible center jump during handoff.
- Removed the Windows sender's center warp and switched edge-session movement to relative Raw Input. Edge entry lands two desktop units inside the receiving edge at the normalized crossing height. The Mac publishes its entry position immediately and restores its return position before showing the cursor.
- Windows workspace checks passed. The native probe verified a quarter-height entry near the left edge without triggering local takeover. Apple Silicon and x86-64 Linux passed cross-target Clippy.
- Next, verify live crossings near the top and bottom in both directions with a relative Windows mouse and Mac trackpad.

## 2026-11-14 — Phase 4 fixed display edge

- Accepted the two-machine input MVP and added opt-in horizontal edge switching for the primary displays of a pinned bidirectional pair. Opposite `peer_side` settings are required. Crossing carries a normalized pointer height, and crossing back returns the original sender's keyboard and pointer locally.
- Windows workspace build, tests, strict Clippy, and desktop probes passed, including native pointer placement without false takeover. Apple Silicon and x86-64 Linux passed cross-target strict Clippy.
- Next, verify both crossings and the emergency shortcut on the paired desktops. The first slice uses one fixed pair of primary displays and requires an agent restart after changing the primary displays. Monitor selection, layout synchronization, and the editor remain open.

## 2026-11-14 — Japanese IME takeover on Windows

- Two Mac-to-Windows tests reproduced a false local takeover when Cmd+Space selected Japanese. Both traces showed an untagged key release at the input-language switch, and the authenticated peer connection stayed up.
- Windows keyboard takeover now requires a key press. Key releases still go through the normal capture path. The regression test verifies that an untagged release leaves the takeover signal unchanged and a press triggers it.
- Windows workspace checks and desktop input probes passed. The operator confirmed the Japanese switch worked over the patched Mac-to-Windows session.

## 2026-11-15 — macOS sender cursor drift recovery

- Fixed visible movement of the Mac sender's cursor by hiding it during capture and returning it to its saved anchor before showing it on release. Capture retains the anchor even after disassociation succeeds and corrects drift on mouse callbacks and monitor polls. Hide/show and reassociation are scoped to capture cleanup.
- Background hiding requires the private, process-local WindowServer property `SetsCursorInBackground`. Its entry points are resolved at runtime; missing support or ineffective hiding rejects capture. No system preferences or dependencies changed.
- On this Apple Silicon Mac, the native regression verified hiding, repeated drift correction, and restored visibility; all 37 standard workspace tests, formatting, strict Clippy, and workspace build passed. The operator confirmed Mac-to-Windows forwarding with the cursor hidden, then restored after Ctrl+Shift+F10.
- Further work: verify foreground changes and cleanup on other macOS versions. This private cursor capability remains a compatibility constraint.

## 2026-11-14 — Display-paced pointer motion

- The paired desktops passed Windows-to-Mac input. Mac-to-Windows input worked with a moving local Mac pointer and visibly slower motion on the 240 Hz Windows monitor.
- Each sender forwards pointer movement directly from its native callback. macOS disassociates or parks its cursor while sending. Both receivers pace motion to the reported rate of their display and apply pending movement before a click. The input wire format is unchanged.
- The Windows desktop probe detected 240 Hz and moved the pointer in paced steps. The 60-to-240 and 240-to-60 Hz tests verify interpolation, coalescing, and preservation of relative motion. The next step on macOS is to run the [refresh probe](platform-support.md#run-the-macos-input-probes) and check cursor parking and motion in both directions on the paired desktops.

## 2026-11-14 — Bidirectional Windows and macOS input

- Added `[input] mode = "bidirectional"` for a selected pinned peer. Each machine reserves local focus for one input session. Physical keyboard, mouse, or trackpad activity on the receiver ends sharing and restores local control. The shortcut starts the next session in either direction.
- Implemented macOS capture through a Quartz session tap and Windows injection through `SendInput`. Both adapters filter synthetic events. The receiver attempts to release held keys and buttons before it grants another session.
- Windows formatting, strict Clippy, workspace build, all 36 standard tests, both native input adapter tests, and the standalone Windows input probe passed. Strict Clippy passed for Apple Silicon and x86-64 Linux as compile-time checks. Cargo deny and the cached RustSec audit passed.
- Next, run the [Mac capture probe](platform-support.md#run-the-macos-input-probes) and the [two-desktop handoff walkthrough](input-control.md). Verify trackpad motion, takeover while a remote modifier or mouse button is held, and recovery after stopping either agent. Native macOS capture, hard receiver termination with held input, and display-edge switching remain open.

## 2026-11-14 — Opt-in Windows-to-Mac input session

- The Windows operator verified physical F9 suppression and Ctrl+Shift+F10 delivery. The Mac injection probe had already passed on the target desktop.
- Added one explicit input peer per agent, a reliable QUIC input stream with a grant and renewable lease, Windows capture with a bounded queue and local watchdog, and Mac synthetic key and button release on stream cleanup. Ctrl+Shift+F9 starts a session and Ctrl+Shift+F10 returns to Windows.
- Windows formatting, strict Clippy, workspace build, and all 32 standard tests passed. The input stream test exercised both QUIC connection roles. The Windows hook installation and mDNS LAN tests passed separately. Cross-target strict Clippy passed for Apple Silicon macOS and x86-64 Linux.
- The operator verified the Windows-to-Mac session on the paired desktops. Held-input cleanup after hard termination still needs verification.

## 2026-11-14 — Phase 3 native input feasibility

- Added a macOS Quartz sink for physical key positions, modifiers, pointer motion, mouse buttons, and scroll. The standalone probe checks posting permission and exercises keyboard input, Shift release, cursor movement, and scrolling on request.
- Extended the Windows probe with an explicit, ten-second physical F9 suppression check and an independent Ctrl+Shift+F10 completion shortcut.
- Windows formatting, strict Clippy, build, all 27 standard workspace tests, and the synthetic Windows input probe passed. Cross-target Clippy passed for Apple Silicon macOS and x86-64 Linux. Cargo deny and the cached RustSec audit passed.
- The Mac operator confirmed the standalone injection exercise passed. The subsequent [Windows physical suppression probe](platform-support.md#run-the-windows-probe) also passed.

## 2026-11-14 — Multiple trusted peers

- Verified PC-to-Mac discovery with the Windows dialer configured without a manual address. Its log recorded the Mac's mDNS address and an authenticated QUIC connection.
- Added multiple approved peers per agent on one QUIC endpoint. Each pair has a deterministic dial direction, one accepted session, and independent reconnect. Existing single-peer configs still load.
- Windows formatting, strict Clippy, workspace build, and 27 standard tests passed. The three-agent loopback test established all three direct pairwise connections, kept the surviving pair connected while the third restarted, and verified both of its connections returned. Duplicate-session and unknown-certificate rejection passed. The Windows multicast test also passed with the updated transport. Cross-target Clippy passed for Apple Silicon macOS and x86-64 Linux.
- The existing Windows–Mac agents are still on the verified discovery build. Next, run the mesh revision on both machines and validate reciprocal connections with a third instance. The next implementation milestone is Phase 3 input control, starting with native input probes and the shortcut-based return-to-local flow.

## 2026-11-14 — mDNS address discovery for an approved peer

- Verified manual pairing on the Windows PC and Mac over the LAN. The Windows agent reported an authenticated connection to the Mac, and the Mac operator confirmed its side.
- Added opt-in DNS-SD on the interface selected by the agent's listen address. The dialer can use mDNS hints for the already pinned peer, alongside an optional manual address. Mutual TLS and explicit certificate approval still govern trust.
- Windows formatting, strict Clippy, workspace build, and 25 standard tests passed. A separate Windows LAN test connected two local QUIC endpoints using multicast DNS alone. Dependency policy and RustSec checks passed. Cross-target Clippy covered Apple Silicon macOS and x86-64 Linux.
- Next, run the [discovery-only walkthrough](manual-pairing.md#discover-the-paired-address) on the Windows PC and Mac. Follow with support for multiple trusted peers and a three-agent continuity test.

## 2026-11-14 — First Phase 2 networking slice

- Pulled the macOS IPC fix at `af268b0`. GitHub CI passed on Windows, macOS, and Linux for that revision.
- Added manually configured two-device connections with mutual TLS 1.3, persistent Ed25519 identities, bilateral SHA-256 fingerprint approval, and automatic reconnect. One certificate per peer and the local config keep trust explicit across restarts.
- Windows GNU passed formatting, strict Clippy, all 22 workspace tests, and build. Cross-target Clippy passed for Apple Silicon macOS and x86-64 Linux. Loopback tests covered both connection directions, unknown client and server rejection, and automatic reconnect after the listener restarted at the same address. A Windows CLI/agent smoke test verified identity creation, export, mismatched fingerprint rejection, and local status with networking enabled. Cargo deny and the cached RustSec audit passed with duplicate transitive version warnings from cargo deny.
- The new networking code still needs macOS and Linux runtime verification. Next, run the [manual pairing walkthrough](manual-pairing.md) between Windows and macOS, including stopping and restarting either machine's agent. Then add mDNS discovery, multiple trusted peers, and a three-agent continuity test. Keyboard and mouse control remains a later milestone.

## 2026-11-14 — macOS local IPC recovery

- Fixed the Unix listener terminating when macOS reports `NotConnected` during peer credential lookup for a client that closed before accept. The listener discards the socket and continues; unverified streams are never served.
- Updated the native transport regression to cover disconnects both before and after accept, followed by repeated successful status queries. Linux may authenticate the abandoned connection and observe EOF instead.
- Native verification on macOS 27.3 (arm64): formatting, Clippy with warnings denied, all 16 workspace tests, and workspace build passed. Separate agent/CLI processes returned status after three clients connected and disconnected without requests.
- Remaining limitations: macOS input probes and other desktop runtime validation remain pending. Next step remains the proposed Phase 2 networking task below.

## 2026-11-14 — Phase 0 and initial Phase 1

- Recorded dependency decisions and a six-desktop feasibility matrix. Added a standalone Windows probe. Desktop access, hotkey registration, cursor placement at the monitor edge, and tagged keyboard and mouse injection, capture, and suppression passed.
- Established a Rust 2024 workspace with a pinned stable toolchain, OS-independent core types, tested geometry and focus transitions, and a versioned TOML config. Added CI for Linux, macOS, and Windows.
- Implemented a headless agent and `unikvm status` over native local IPC. Windows uses a logon-scoped named pipe and Linux and macOS use per-user Unix sockets. A successful query returns the agent's actual PID and uptime.

### Verification

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
cargo build --locked --workspace
cargo run --locked -p unikvm-platform --example input_probe
cargo deny --offline check
cargo audit --no-fetch --no-yanked
```

Windows GNU build, formatting, Clippy, and all 16 tests passed. Separate agent and CLI processes passed repeated status queries, duplicate-agent rejection, shutdown detection after process termination, and restart. Cross-target Clippy passed for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`. These checks type-check the code and tests for those targets. Native CI runs are still pending.

Dependency checks used fetched advisory data. Cargo deny passed advisories, yanked versions, dependency sources and third-party licenses, with a warning for the two transitive `syn` major versions. Cargo audit passed its advisory scan with yanked-version checking delegated to cargo deny. Workspace packages are unpublished. Third-party license checks use an allowlist, and the project license choice is open.

### Remaining work and next step

Run the physical-input and hotkey test on Windows, including higher-integrity and mixed-DPI cases. Run native IPC tests and targeted desktop probes on macOS, X11, GNOME Wayland, KDE Wayland, and Sway. Phase 0 runtime validation remains open for those desktops. The initial Phase 1 scope ends at CLI status. The Ratatui peer view needs a real peer feed.

**Proposed first Phase 2 task.** Implement a two-agent integration test around a Quinn endpoint per agent and manually configured loopback addresses. Provision a persistent random device ID and a TLS identity per agent. Use an explicit fingerprint comparison and bilateral pinning flow. Verify that each agent can dial and accept, that an untrusted peer is rejected, and that a trusted peer reconnects after restart. Follow with mDNS discovery, simultaneous-dial deduplication, a three-agent continuity test, and a real peer feed for the TUI.
