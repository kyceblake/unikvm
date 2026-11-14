# Windows locked receiver

The current Windows build supports input on the ordinary logged-in desktop. It
does **not** include a privileged secure-desktop helper. Setting
`input.allow_locked_receive = true` does not grant that capability: an incoming
session on an inaccessible desktop returns an explicit unsupported error.

The existing capture monitor releases outgoing control when the input desktop
changes and keeps polling for the ordinary desktop to return. This preserves
normal lock/unlock recovery for an already running agent. Starting capture while
the desktop is inaccessible still fails; the network agent must not be reported
as a working lock-screen receiver merely because local status responds.

## Capability-only native probe

`secure_desktop_probe` exercises the service-to-console-session boundary before
any privileged input implementation is enabled. It never injects input, receives
input events, listens on the network, or changes desktop ACLs. A one-shot
LocalSystem service duplicates its own token into the active console session,
launches its own worker on `WinSta0\Winlogon`, and reads a bounded capability
report through a SYSTEM-only local pipe. The two endpoints check their expected
parent/child process IDs. Results go to the Application event log; no key state,
user names, passwords, or credentials are reported.

From the Windows repository root, build using the pinned toolchain:

```powershell
cargo build --locked -p unikvm-platform --example secure_desktop_probe
```

Review the probe source and installer before executing these commands in an
**Administrator PowerShell** window. Installation copies only this separate probe
executable under Program Files, restricts replacement to SYSTEM/Administrators,
and registers a manual LocalSystem service. It does not elevate the network agent.

```powershell
.\scripts\windows-secure-desktop-probe.ps1 -Action Install
.\scripts\windows-secure-desktop-probe.ps1 -Action Run -DelaySeconds 15
```

Press **Win+L during the countdown**. Wait about 20 seconds, then unlock Windows
with its physical keyboard. Read the report after unlocking:

```powershell
.\scripts\windows-secure-desktop-probe.ps1 -Action Report
```

A locked-console feasibility result should include:

```text
service_system=true
worker_system=true
worker_session=<same value as requested_console_session>
worker_station=WinSta0
worker_desktop=Winlogon
session_locked=true
input_desktop=Winlogon
input_injected=false
```

This proves worker attachment and desktop observation only. It does not prove
input injection or establish the later console-user IPC authorization boundary.
If the probe fails, retain its capability/error report before changing the design.
Windows type checking and strict Clippy passed; native execution remains pending.

After collecting the report, remove the temporary service and copied binary:

```powershell
.\scripts\windows-secure-desktop-probe.ps1 -Action Remove
```

Event history and existing pairing data remain intact.

## Required service boundary

Implement this as a separate, optional privileged injector. Keep networking,
certificate identities, peer approval, and the existing local agent in the user's
session. The privileged component must never expose a network listener.

1. An administrator installs a dedicated LocalSystem service and its worker under
   an administrator-writable installation directory. The service launches only
   its own worker executable from that protected location. Neither executable
   accepts a caller-supplied executable path or shell command.
2. The service identifies the active physical console session and its existing
   logged-in user. It duplicates its own SYSTEM token, sets the worker's session
   to that console session, and launches a dedicated injector attached to that
   session's `WinSta0\Winlogon` desktop. Service Session 0 is not the interactive
   user's session. A service cannot fix this by calling `SetThreadDesktop` alone.
3. Use named IPC with explicit SYSTEM/current-console-logon-SID ACLs and remote
   clients disabled. Handles cannot simply be inherited across sessions. The
   worker verifies the kernel-provided client token, session, and logon SID; the
   user agent verifies that its server is owned by SYSTEM before sending input.
4. The agent invokes the helper only for the configured, authenticated input
   peer after granting an opted-in receive session. IPC carries bounded,
   versioned input records, renewals, and release requests. It carries no network
   credentials, executable names, file paths, arbitrary commands, or key logs.
5. The injector independently verifies the active console session, the user
   identity, actual session-locked state, and active Winlogon desktop throughout
   the lease. A UAC prompt or another secure desktop must not be accepted as a
   normal lock screen merely because the ordinary desktop is unavailable.
6. Only one receiving session may own synthetic state. Disconnect, lease expiry,
   service stop, console-session switch, unlock, and logoff must release recorded
   keys/buttons and close the worker connection. Desktop-switch cleanup needs
   native verification; successful `SendInput` on one desktop does not establish
   cleanup behavior after the desktop has changed.

Windows installation, service privileges, and binary ACLs require explicit
administrator setup. Do not weaken the Winlogon desktop ACL, disable UAC/Secure
Desktop, or run the entire network agent as SYSTEM to avoid this boundary.

## Native acceptance checks

These are required before claiming locked Windows receiving support:

- The console user's agent can connect; a different user/session, remote named
  pipe client, or ordinary process forging the helper endpoint is rejected.
- The helper rejects unlocked desktops, UAC prompts, inactive sessions, logoff,
  and sessions belonging to a different console user.
- From an unlocked Mac, the approved peer can move/click on the normal Windows
  lock screen and type the operator's password. The operator enters credentials;
  neither tests nor logs record them. Domain policies requiring Secure Attention
  Sequence need separate handling; `SendInput` cannot generate Ctrl+Alt+Del.
- Unlock, sender disconnect, worker termination, service stop, lease expiry, and
  console-user changes recover local control without held modifiers or buttons.
- Ordinary unlocked input still uses the existing unprivileged backend. Locked
  sender forwarding is not enabled by this receiver capability.

Cross-target compilation verifies Windows types and APIs only. It cannot verify
service installation, token privileges, session isolation, Winlogon access, or
the native input/cleanup behavior above.

## Primary references

- [CreateProcessAsUserW: token session, desktop, and cross-session handle rules](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw)
- [WTSQueryUserToken: LocalSystem/TCB requirements](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsqueryusertoken)
- [SetThreadDesktop: current-window-station constraint](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)
- [Windows authentication and the Winlogon secure desktop](https://learn.microsoft.com/en-us/windows-server/security/windows-authentication/credentials-processes-in-windows-authentication)
- [SendInput: injection restrictions](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
