//! LocalSystem launcher and SYSTEM-only result channel for a Winlogon worker.
//! No input commands, network listener, credential capture, or desktop ACL edits.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use windows_sys::Win32::Foundation::{
    ERROR_NOT_ALL_ASSIGNED, ERROR_SERVICE_SPECIFIC_ERROR, GetLastError, LUID, LocalFree,
    SetLastError, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, DuplicateTokenEx, GetTokenInformation, IsWellKnownSid,
    LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SECURITY_ATTRIBUTES,
    SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_PRIVILEGES, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER, TokenPrimary, TokenSessionId,
    TokenUser, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::EventLog::{
    DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE, RegisterEventSourceW,
    ReportEventW,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSGetActiveConsoleSessionId,
    WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerW, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetThreadDesktop,
    GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, GetCurrentProcessId,
    GetCurrentThreadId, OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess,
    WaitForSingleObject,
};

const SERVICE: &str = "UnikvmSecureDesktopProbe";
const REPORT_LIMIT: usize = 2048;
const DEADLINE: Duration = Duration::from_secs(15);
static STOP: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode] if mode == "--service" => dispatch(),
        [mode, pipe, session, parent] if mode == "--worker" => {
            let session = parse_number(session)?;
            let parent = parse_number(parent)?;
            require_system()?;
            runtime()?.block_on(worker(pipe, session, parent))
        }
        [] => {
            println!("Windows secure-desktop feasibility probe; never injects input.");
            println!("Use scripts/windows-secure-desktop-probe.ps1 for explicit service setup.");
            Ok(())
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: secure_desktop_probe [--service]",
        )),
    }
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn dispatch() -> io::Result<()> {
    let mut name = wide(SERVICE);
    let entries = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: Names and the table remain live until the dispatcher returns.
    check(unsafe { StartServiceCtrlDispatcherW(entries.as_ptr()) })
}

unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let name = wide(SERVICE);
    // SAFETY: The SCM invokes this entry point; handler is a static callback.
    let handle = unsafe { RegisterServiceCtrlHandlerW(name.as_ptr(), Some(service_control)) };
    if handle.is_null() {
        return;
    }
    STATUS.store(handle as usize, Ordering::Release);
    status(SERVICE_START_PENDING, false);
    let result = require_system().and_then(|()| {
        status(SERVICE_RUNNING, false);
        runtime()?.block_on(probe())
    });
    let failed = match &result {
        Ok(report) => report_event(false, report).is_err(),
        Err(error) => {
            // Only capability/OS error context is emitted; never input or credentials.
            let _ = report_event(true, &format!("secure-desktop probe failed: {error}"));
            true
        }
    };
    // Give SCM clients time to observe RUNNING for this one-shot probe rather
    // than mistaking a quickly completed worker for a service start failure.
    std::thread::sleep(Duration::from_secs(1));
    status(SERVICE_STOPPED, failed);
}

unsafe extern "system" fn service_control(control: u32) {
    if control == SERVICE_CONTROL_STOP {
        STOP.store(true, Ordering::Release);
        status(SERVICE_STOP_PENDING, false);
    }
}

fn status(state: u32, failed: bool) {
    let handle = STATUS.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP
        } else {
            0
        },
        dwWin32ExitCode: if failed {
            ERROR_SERVICE_SPECIFIC_ERROR
        } else {
            0
        },
        dwServiceSpecificExitCode: u32::from(failed),
        dwWaitHint: if matches!(state, SERVICE_START_PENDING | SERVICE_STOP_PENDING) {
            20_000
        } else {
            0
        },
        ..Default::default()
    };
    // SAFETY: Only the SCM-returned status handle is stored in STATUS.
    let _ = unsafe { SetServiceStatus(handle, &status) };
}

async fn probe() -> io::Result<String> {
    // SAFETY: This argument-free API returns the active physical console session.
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX || session == 0 {
        return Err(io::Error::other("no nonzero active console session"));
    }
    // Service PID prevents collisions; first-instance creation rejects precreation.
    let parent = unsafe { GetCurrentProcessId() };
    let name = format!(r"\\.\pipe\unikvm-secure-probe-{parent}-{session}");
    let mut descriptor = LocalAllocation(null_mut());
    let sddl = wide("O:SYG:SYD:P(A;;GA;;;SY)");
    // SAFETY: SDDL is terminated and descriptor is an output allocation.
    check(unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor.0,
            null_mut(),
        )
    })?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // SAFETY: The descriptor and attributes live through synchronous creation.
    let mut pipe = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .in_buffer_size(REPORT_LIMIT as u32)
            .out_buffer_size(64)
            .create_with_security_attributes_raw(&name, (&raw const attributes).cast_mut().cast())?
    };
    let child = launch_worker(session, &name, parent)?;
    let receive = async {
        pipe.connect().await?;
        let mut client = 0;
        // SAFETY: The server owns this connected pipe; client is writable output.
        check(unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut client) })?;
        if client != child.pid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unexpected worker PID",
            ));
        }
        let size = pipe.read_u32_le().await? as usize;
        if size == 0 || size > REPORT_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid capability report size",
            ));
        }
        let mut bytes = vec![0; size];
        pipe.read_exact(&mut bytes).await?;
        String::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid capability report"))
    };
    let report = tokio::select! {
        result = tokio::time::timeout(DEADLINE, receive) => {
            result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "worker result timed out"))??
        }
        () = stopped() => return Err(io::Error::new(io::ErrorKind::Interrupted, "probe stopped")),
    };
    // The child owns no input state, and Drop terminates it if it did not exit.
    Ok(format!(
        "service_system=true\nrequested_console_session={session}\n{report}"
    ))
}

async fn stopped() {
    while !STOP.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn launch_worker(session: u32, pipe: &str, parent: u32) -> io::Result<Child> {
    let token = process_token(TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_PRIVILEGES)?;
    enable_privilege(&token, "SeTcbPrivilege")?;
    let mut raw = null_mut();
    // SAFETY: Duplicate our own SYSTEM token; no token is borrowed from another process.
    check(unsafe {
        DuplicateTokenEx(
            token.as_raw_handle(),
            TOKEN_ALL_ACCESS,
            null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut raw,
        )
    })
    .map_err(|error| at("DuplicateTokenEx(SYSTEM)", error))?;
    let primary = owned(raw);
    // SAFETY: Session is a scalar TokenSessionId value for the owned duplicate.
    check(unsafe {
        SetTokenInformation(
            primary.as_raw_handle(),
            TokenSessionId,
            (&raw const session).cast(),
            size_of::<u32>() as u32,
        )
    })
    .map_err(|error| at("SetTokenInformation(TokenSessionId)", error))?;
    let executable = std::env::current_exe()?;
    let path = executable
        .to_str()
        .ok_or_else(|| io::Error::other("non-Unicode executable path"))?;
    if path.contains('"') {
        return Err(io::Error::other("invalid executable path"));
    }
    let application = wide(path);
    let mut command = wide(&format!("\"{path}\" --worker {pipe} {session} {parent}"));
    let mut desktop = wide(r"winsta0\winlogon");
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: desktop.as_mut_ptr(),
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: All buffers stay live for creation. Explicit application path avoids
    // command-line executable ambiguity. Cross-session handle inheritance is OFF.
    check(unsafe {
        CreateProcessAsUserW(
            primary.as_raw_handle(),
            application.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_NO_WINDOW,
            null(),
            null(),
            &startup,
            &mut process,
        )
    })
    .map_err(|error| at("CreateProcessAsUserW(WinSta0\\Winlogon)", error))?;
    let _thread = owned(process.hThread);
    Ok(Child {
        handle: owned(process.hProcess),
        pid: process.dwProcessId,
    })
}

async fn worker(pipe: &str, expected_session: u32, parent: u32) -> io::Result<()> {
    // Only a service-launched SYSTEM process in the active console may report.
    let mut session = 0;
    // SAFETY: Current PID is valid and session is writable output.
    check(unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) })?;
    if session != expected_session || session != unsafe { WTSGetActiveConsoleSessionId() } {
        return Err(io::Error::other(
            "worker is not in the requested console session",
        ));
    }
    // SAFETY: These APIs return borrowed handles for this process/thread.
    let station = object_name(unsafe { GetProcessWindowStation() })?;
    let desktop = object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })?;
    if !station.eq_ignore_ascii_case("WinSta0") || !desktop.eq_ignore_ascii_case("Winlogon") {
        return Err(io::Error::other(
            "worker did not attach to WinSta0\\Winlogon",
        ));
    }
    // The capability report contains no user identity, key state, text, or credentials.
    let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    let input_desktop = if input.is_null() {
        format!(
            "unavailable:{}",
            io::Error::last_os_error().raw_os_error().unwrap_or(0)
        )
    } else {
        let name = object_name(input);
        // SAFETY: This is the owned handle returned by OpenInputDesktop.
        unsafe { CloseDesktop(input) };
        name?
    };
    let locked = session_locked(session)
        .map(|value| value.to_string())
        .unwrap_or_else(|error| format!("unknown:{}", error.raw_os_error().unwrap_or(0)));
    let report = format!(
        "worker_system=true\nworker_session={session}\nworker_station={station}\nworker_desktop={desktop}\nsession_locked={locked}\ninput_desktop={input_desktop}\ninput_injected=false\n"
    );
    let mut client = ClientOptions::new()
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(pipe)?;
    let mut server = 0;
    // SAFETY: Connected client handle is live and server is writable output.
    check(unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut server) })?;
    if server != parent {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unexpected service PID",
        ));
    }
    tokio::time::timeout(DEADLINE, async {
        client.write_u32_le(report.len() as u32).await?;
        client.write_all(report.as_bytes()).await?;
        client.shutdown().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "worker report write timed out"))?
}

fn session_locked(session: u32) -> io::Result<bool> {
    let mut buffer = null_mut();
    let mut bytes = 0;
    // SAFETY: Query an owned WTS allocation for the specified console session.
    check(unsafe {
        WTSQuerySessionInformationW(
            null_mut(),
            session,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        )
    })?;
    struct Wts(*mut u16);
    impl Drop for Wts {
        fn drop(&mut self) {
            // SAFETY: WTS allocated this buffer.
            unsafe { WTSFreeMemory(self.0.cast()) };
        }
    }
    let buffer = Wts(buffer);
    if (bytes as usize) < size_of::<WTSINFOEXW>() {
        return Err(io::Error::other("short WTS session information"));
    }
    // SAFETY: WTS reports a complete WTSINFOEXW in the checked allocation.
    let info = unsafe { &*buffer.0.cast::<WTSINFOEXW>() };
    if info.Level != 1 {
        return Err(io::Error::other("unknown WTS session information level"));
    }
    // SAFETY: Level 1 selects this documented union member.
    Ok(unsafe { info.Data.WTSInfoExLevel1.SessionFlags as u32 } == WTS_SESSIONSTATE_LOCK)
}

fn process_token(access: u32) -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    // SAFETY: GetCurrentProcess is a valid pseudohandle; token is writable output.
    check(unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) })?;
    Ok(owned(token))
}

fn require_system() -> io::Result<()> {
    let token = process_token(TOKEN_QUERY)?;
    let mut bytes = [0usize; 64];
    let mut size = 0;
    // SAFETY: Pointer-aligned storage is large enough for TOKEN_USER and one SID.
    check(unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            bytes.as_mut_ptr().cast(),
            size_of::<[usize; 64]>() as u32,
            &mut size,
        )
    })?;
    // SAFETY: A successful TokenUser query filled this structure and its SID.
    let user = unsafe { &*bytes.as_ptr().cast::<TOKEN_USER>() };
    if unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "probe service/worker requires LocalSystem",
        ));
    }
    Ok(())
}

fn enable_privilege(token: &OwnedHandle, name: &str) -> io::Result<()> {
    let name = wide(name);
    let mut luid = LUID::default();
    // SAFETY: Name is terminated and luid is writable output.
    check(unsafe { LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) })?;
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: Owned current-process token has TOKEN_ADJUST_PRIVILEGES access.
    unsafe { SetLastError(0) };
    check(unsafe {
        AdjustTokenPrivileges(
            token.as_raw_handle(),
            0,
            &privileges,
            0,
            null_mut(),
            null_mut(),
        )
    })?;
    // AdjustTokenPrivileges can succeed while failing to assign the privilege.
    if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
        return Err(io::Error::other("SYSTEM token lacks required privilege"));
    }
    Ok(())
}

fn object_name(handle: *mut c_void) -> io::Result<String> {
    let mut name = [0u16; 256];
    let mut bytes = 0;
    // SAFETY: A borrowed desktop/window-station handle and writable output buffer.
    check(unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            size_of::<[u16; 256]>() as u32,
            &mut bytes,
        )
    })?;
    let end = name
        .iter()
        .position(|&unit| unit == 0)
        .ok_or_else(|| io::Error::other("unterminated desktop name"))?;
    String::from_utf16(&name[..end]).map_err(|_| io::Error::other("invalid desktop name"))
}

fn report_event(failed: bool, report: &str) -> io::Result<()> {
    let name = wide(SERVICE);
    // SAFETY: Static source name is terminated. Handle is closed below.
    let handle = unsafe { RegisterEventSourceW(null(), name.as_ptr()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let value = wide(report);
    let strings = [value.as_ptr()];
    // SAFETY: The one bounded string and handle stay live throughout the call.
    let result = check(unsafe {
        ReportEventW(
            handle,
            if failed {
                EVENTLOG_ERROR_TYPE
            } else {
                EVENTLOG_INFORMATION_TYPE
            },
            0,
            if failed { 1001 } else { 1000 },
            null_mut(),
            1,
            0,
            strings.as_ptr(),
            null(),
        )
    });
    // SAFETY: RegisterEventSource returned this owning event-log handle.
    unsafe { DeregisterEventSource(handle) };
    result
}

struct Child {
    handle: OwnedHandle,
    pid: u32,
}
impl Drop for Child {
    fn drop(&mut self) {
        // This probe's worker never owns input state. Do not leave an orphan on
        // timeout, service stop, or malformed result; terminate only our own child.
        if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 500) } == WAIT_TIMEOUT {
            unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) };
        }
    }
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: The security-descriptor API allocated this block with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

fn owned(handle: *mut c_void) -> OwnedHandle {
    // SAFETY: Callers pass newly created, valid, uniquely owned Win32 handles.
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn parse_number(value: &str) -> io::Result<u32> {
    value
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid session/process ID"))
}

fn check(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn at(operation: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{operation}: {error}"))
}
