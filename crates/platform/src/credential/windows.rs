#![allow(unsafe_code)]

use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use zeroize::{Zeroize, Zeroizing};

pub(super) fn protect(secret: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    transform(secret, true)
}

pub(super) fn unprotect(sealed: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    transform(sealed, false)
}

fn transform(bytes: &[u8], protect: bool) -> io::Result<Zeroizing<Vec<u8>>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(io::Error::other)?,
        // DPAPI reads the input bytes. The Win32 blob field itself uses a mutable pointer.
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: The blobs, input bytes and output pointer are live. Optional inputs are null.
    let result = unsafe {
        if protect {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.pbData.is_null() {
        return Ok(Zeroizing::new(Vec::new()));
    }
    // SAFETY: Successful DPAPI returned an owned allocation of cbData bytes.
    let data = unsafe { std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize) };
    let result = Zeroizing::new(data.to_vec());
    data.zeroize();
    // SAFETY: DPAPI allocates the output with LocalAlloc and the slice is no longer in use.
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(result)
}
