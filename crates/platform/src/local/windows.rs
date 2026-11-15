#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::mem::size_of_val;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::time::Duration;

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, TOKEN_GROUPS,
    TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER, TokenLogonSid, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub struct Endpoint {
    name: String,
    user: String,
    logon: String,
}

pub struct Listener {
    pipe: NamedPipeServer,
}

impl Endpoint {
    pub fn for_current_user() -> io::Result<Self> {
        let (user, logon) = current_sids()?;
        let name = format!(r"\\.\pipe\unikvm-{logon}");
        Ok(Self { name, user, logon })
    }

    #[cfg(test)]
    pub(super) fn for_test(namespace: &str) -> io::Result<Self> {
        let mut endpoint = Self::for_current_user()?;
        endpoint.name.push('-');
        endpoint.name.push_str(namespace);
        Ok(endpoint)
    }

    pub fn listen(self) -> io::Result<Listener> {
        let pipe = self.create_pipe()?;
        Ok(Listener { pipe })
    }

    #[cfg(feature = "clipboard-monitor")]
    pub(crate) fn tray_name(&self) -> String {
        format!("Local\\UniKVM-Tray-{}", self.logon)
    }

    pub async fn connect(&self) -> io::Result<NamedPipeClient> {
        let client = loop {
            match ClientOptions::new()
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&self.name)
            {
                Ok(client) => break client,
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        };
        if pipe_owner(&client)? != self.user {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "agent pipe belongs to another user",
            ));
        }
        Ok(client)
    }

    fn create_pipe(&self) -> io::Result<NamedPipeServer> {
        let sddl: Vec<u16> = format!("O:{}D:P(A;;GA;;;{})", self.user, self.logon)
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = LocalAllocation(null_mut());
        // SAFETY: SDDL is NUL-terminated and descriptor is a writable output pointer.
        check(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor.0,
                null_mut(),
            )
        })?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of_val(&SECURITY_ATTRIBUTES::default()) as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: The attributes and valid descriptor live through the synchronous create call.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .max_instances(1)
                .in_buffer_size(256)
                .out_buffer_size(256)
                .create_with_security_attributes_raw(
                    &self.name,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
        }
    }
}

impl Listener {
    pub async fn accept(&mut self) -> io::Result<&mut NamedPipeServer> {
        self.pipe.connect().await?;
        Ok(&mut self.pipe)
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.pipe.disconnect()
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: The pointer is null or owns an allocation returned by a Win32 API using LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

fn current_sids() -> io::Result<(String, String)> {
    let mut token = null_mut();
    // SAFETY: GetCurrentProcess is a valid pseudohandle and token is a writable output.
    check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
    // SAFETY: A successful OpenProcessToken returns a uniquely owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let user = token_info(&token, TokenUser)?;
    let logon = token_info(&token, TokenLogonSid)?;
    // SAFETY: The buffers are pointer-aligned and were filled with these matching structures.
    // The SID pointers remain valid until the respective buffers are dropped.
    let (user, logon) = unsafe {
        (
            &*user.as_ptr().cast::<TOKEN_USER>(),
            &*logon.as_ptr().cast::<TOKEN_GROUPS>(),
        )
    };
    if logon.GroupCount != 1 {
        return Err(io::Error::other("Windows token has no unique logon SID"));
    }
    Ok((sid_string(user.User.Sid)?, sid_string(logon.Groups[0].Sid)?))
}

fn token_info(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Box<[usize]>> {
    // Both queries return one SID. This capacity covers the fixed header and maximum SID length.
    // Heap allocation keeps the absolute SID pointers valid when this buffer is returned.
    let mut buffer = vec![0usize; 64].into_boxed_slice();
    let mut length = 0;
    // SAFETY: The token is live and the output buffer is pointer-aligned with the supplied capacity.
    check(unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            size_of_val(&*buffer) as u32,
            &mut length,
        )
    })?;
    Ok(buffer)
}

fn pipe_owner(client: &NamedPipeClient) -> io::Result<String> {
    let mut owner = null_mut();
    let mut descriptor = LocalAllocation(null_mut());
    // SAFETY: client owns the live pipe handle. Both requested outputs are writable; the returned
    // SID is kept alive by descriptor through conversion.
    let error = unsafe {
        GetSecurityInfo(
            client.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor.0,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    sid_string(owner)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    if sid.is_null() {
        return Err(io::Error::other(
            "Windows returned an empty security identifier",
        ));
    }
    let mut string = null_mut();
    // SAFETY: The caller supplies a live SID from a token or security descriptor.
    check(unsafe { ConvertSidToStringSidW(sid, &mut string) })?;
    let _allocation = LocalAllocation(string.cast());
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated UTF-16 allocation.
    let wide = unsafe { decode_wide_sid(string) };
    wide.map_err(|_| io::Error::other("invalid Windows security identifier"))
}

unsafe fn decode_wide_sid(ptr: *const u16) -> Result<String, std::string::FromUtf16Error> {
    // SAFETY: The caller keeps the NUL-terminated allocation live during conversion.
    let len = unsafe { (0..).take_while(|&i| *ptr.add(i) != 0).count() };
    // SAFETY: len counts only units before the NUL in the same allocation.
    String::from_utf16(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
