#![allow(unsafe_code)]

use std::io;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

use super::{Snapshot, WriteOutcome, cf_html};
use crate::input::windows::check_input_desktop;

pub struct Clipboard {
    exclude: u32,
    history: u32,
    cloud: u32,
    html: u32,
}

impl Clipboard {
    pub fn new() -> io::Result<Self> {
        let exclude = format("ExcludeClipboardContentFromMonitorProcessing")?;
        let history = format("CanIncludeInClipboardHistory")?;
        let cloud = format("CanUploadToCloudClipboard")?;
        let html = format("HTML Format")?;
        Ok(Self {
            exclude,
            history,
            cloud,
            html,
        })
    }

    pub fn available(&self) -> bool {
        check_input_desktop().is_ok()
    }

    pub fn sequence(&self) -> io::Result<u64> {
        // SAFETY: Reads the sequence number for the current window station.
        Ok(u64::from(unsafe { GetClipboardSequenceNumber() }))
    }

    pub fn read(
        &self,
        previous: u64,
        text_limit: usize,
        html_limit: usize,
    ) -> io::Result<Option<Snapshot>> {
        if self.sequence()? == previous {
            return Ok(None);
        }
        let _clipboard = Open::new(null_mut())?;
        let sequence = self.sequence()?;
        if sequence == previous {
            return Ok(None);
        }
        // SAFETY: All formats are registered or standard, and the clipboard is open.
        let (text, html) = unsafe {
            if IsClipboardFormatAvailable(self.exclude) != 0
                || IsClipboardFormatAvailable(u32::from(CF_HDROP)) != 0
                || opt_out(self.history)
                || opt_out(self.cloud)
                || IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) == 0
            {
                (None, None)
            } else {
                let text = read_text(text_limit)?;
                let html = text.as_ref().and_then(|_| read_html(self.html, html_limit));
                (text, html)
            }
        };
        Ok(Some(Snapshot {
            sequence: self.sequence()?,
            text,
            html,
        }))
    }

    pub fn write(&self, expected: u64, text: &str, html: Option<&str>) -> io::Result<WriteOutcome> {
        let utf16: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let data = Memory::new(&utf16)?;
        let excluded = Memory::new(&[0u8])?;
        let html = html
            .map(|text| Memory::new(cf_html::encode(text).as_bytes()))
            .transpose()?;
        let owner = Owner::new()?;
        let _clipboard = Open::new(owner.0)?;
        if self.sequence()? != expected {
            return Ok(WriteOutcome::Changed);
        }
        // SAFETY: This thread owns an open clipboard with a valid owner window.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(io::Error::last_os_error());
        }
        excluded.publish(self.exclude)?;
        if let Some(html) = html {
            // Publish the richest recognized format first. Plain text still works
            // if the optional format cannot be published.
            let _ = html.publish(self.html);
        }
        data.publish(u32::from(CF_UNICODETEXT))?;
        // Record the sequence under the clipboard lock. Windows may advance
        // it on close. The exclusion marker suppresses that notification
        // without also hiding a local copy that wins the race after close.
        Ok(WriteOutcome::Applied(self.sequence()?))
    }
}

unsafe fn read_html(format: u32, limit: usize) -> Option<String> {
    // SAFETY: The caller holds the clipboard lock. GlobalSize bounds the
    // readable bytes, and the returned string owns its data before unlock.
    unsafe {
        if IsClipboardFormatAvailable(format) == 0 {
            return None;
        }
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let bytes = GlobalSize(handle).min(limit.saturating_add(cf_html::MAX_HEADER_BYTES));
        let data = GlobalLock(handle).cast::<u8>();
        if data.is_null() {
            return None;
        }
        let fragment =
            cf_html::decode(std::slice::from_raw_parts(data, bytes), limit).map(str::to_owned);
        GlobalUnlock(handle);
        fragment
    }
}

struct Owner(HWND);

impl Owner {
    fn new() -> io::Result<Self> {
        let class: Vec<_> = "STATIC".encode_utf16().chain([0]).collect();
        // SAFETY: Create a message-only window on the calling desktop thread.
        let owner = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                std::ptr::null(),
            )
        };
        if owner.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(owner))
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: The owner window belongs to the thread that created this object.
        unsafe { DestroyWindow(self.0) };
    }
}

fn format(name: &str) -> io::Result<u32> {
    let name: Vec<_> = name.encode_utf16().chain([0]).collect();
    // SAFETY: The name is a live NUL-terminated UTF-16 string.
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    if format == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(format)
    }
}

struct Open;

impl Open {
    fn new(owner: HWND) -> io::Result<Self> {
        // SAFETY: The owner is a live window or null for read-only access.
        if unsafe { OpenClipboard(owner) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: Balance a successful open on this thread.
        unsafe { CloseClipboard() };
    }
}

unsafe fn opt_out(format: u32) -> bool {
    // SAFETY: Caller has opened the clipboard. An unreadable marker is treated
    // as an opt-out. The clipboard owns the memory.
    unsafe {
        if IsClipboardFormatAvailable(format) == 0 {
            return false;
        }
        let handle = GetClipboardData(format);
        if handle.is_null() || GlobalSize(handle) < size_of::<u32>() {
            return true;
        }
        let data = GlobalLock(handle).cast::<u32>();
        if data.is_null() {
            return true;
        }
        let excluded = data.read_unaligned() != 1;
        GlobalUnlock(handle);
        excluded
    }
}

unsafe fn read_text(limit: usize) -> io::Result<Option<String>> {
    // SAFETY: The open clipboard owns and keeps the global block live. Bound
    // the readable units by its size and the maximum possible accepted text.
    unsafe {
        let handle = GetClipboardData(u32::from(CF_UNICODETEXT));
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let units = (GlobalSize(handle) / size_of::<u16>()).min(limit.saturating_add(1));
        let data = GlobalLock(handle).cast::<u16>();
        if data.is_null() {
            return Err(io::Error::last_os_error());
        }
        let units = std::slice::from_raw_parts(data, units);
        let text = units
            .iter()
            .position(|&unit| unit == 0)
            .and_then(|end| String::from_utf16(&units[..end]).ok())
            .filter(|text| text.len() <= limit);
        GlobalUnlock(handle);
        Ok(text)
    }
}

struct Memory(HGLOBAL);

impl Memory {
    fn new<T: Copy>(data: &[T]) -> io::Result<Self> {
        // SAFETY: Allocate writable movable memory and copy exactly the
        // initialized bytes of the primitive UTF-16 or marker slice.
        unsafe {
            let size = std::mem::size_of_val(data);
            let handle = GlobalAlloc(GMEM_MOVEABLE, size);
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let memory = Self(handle);
            let target = GlobalLock(handle).cast::<u8>();
            if target.is_null() {
                return Err(io::Error::last_os_error());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), target, size);
            GlobalUnlock(handle);
            Ok(memory)
        }
    }

    fn publish(self, format: u32) -> io::Result<()> {
        // SAFETY: The caller owns the open clipboard and the movable block.
        if unsafe { SetClipboardData(format, self.0) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: Only untransferred blocks reach this destructor.
        unsafe { GlobalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::DataExchange::{CountClipboardFormats, EnumClipboardFormats};
    use windows_sys::Win32::System::Ole::{CF_LOCALE, CF_OEMTEXT, CF_TEXT};

    #[test]
    #[ignore = "requires an interactive desktop with an empty clipboard"]
    fn native_unicode_write_is_marked_and_cannot_overwrite_a_newer_copy() {
        const TEXT: &str = "unikvm clipboard regression 日本語\r\n🧪";
        let clipboard = Clipboard::new().unwrap();
        assert!(clipboard.available());
        let (empty, had_empty_text) = {
            let _open = Open::new(null_mut()).unwrap();
            // SAFETY: The clipboard is open on this thread.
            let has_formats = unsafe { CountClipboardFormats() } != 0;
            if has_formats {
                let mut format = 0;
                loop {
                    // SAFETY: Enumerate the formats on the currently open clipboard.
                    format = unsafe { EnumClipboardFormats(format) };
                    if format == 0 {
                        break;
                    }
                    assert!(
                        u16::try_from(format).is_ok_and(|format| matches!(
                            format,
                            CF_UNICODETEXT | CF_TEXT | CF_OEMTEXT | CF_LOCALE
                        )),
                        "empty the clipboard first"
                    );
                }
                // SAFETY: The clipboard is still open and contains only text formats.
                assert!(
                    unsafe { read_text(0) }
                        .unwrap()
                        .is_some_and(|text| text.is_empty()),
                    "empty the clipboard first"
                );
            }
            (clipboard.sequence().unwrap(), has_formats)
        };
        struct Restore<'a>(&'a Clipboard, bool);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                if let Ok(owner) = Owner::new()
                    && let Ok(_open) = Open::new(owner.0)
                {
                    // SAFETY: The clipboard is open. Clear only the marked test text.
                    unsafe {
                        if IsClipboardFormatAvailable(self.0.exclude) == 0
                            || read_text(64 * 1024).ok().flatten().as_deref() != Some(TEXT)
                        {
                            return;
                        }
                        if let Ok(empty) = self.1.then(|| Memory::new(&[0u16])).transpose() {
                            EmptyClipboard();
                            if let Some(data) = empty {
                                let _ = data.publish(u32::from(CF_UNICODETEXT));
                            }
                        }
                    }
                }
            }
        }
        let _restore = Restore(&clipboard, had_empty_text);
        let html = "<strong>日本語🧪</strong>";
        let WriteOutcome::Applied(_) = clipboard.write(empty, TEXT, Some(html)).unwrap() else {
            panic!("clipboard changed before the test write");
        };
        assert!(matches!(
            clipboard.write(empty, "stale", None).unwrap(),
            WriteOutcome::Changed
        ));
        let snapshot = clipboard
            .read(empty, 64 * 1024, 64 * 1024)
            .unwrap()
            .unwrap();
        assert!(
            snapshot.text.is_none(),
            "remote text must not be reread for sync"
        );
        assert!(snapshot.html.is_none());
        let _open = Open::new(null_mut()).unwrap();
        // SAFETY: Read the test string directly from the currently open clipboard.
        assert!(unsafe { read_text(64 * 1024) }.unwrap().as_deref() == Some(TEXT));
        assert!(unsafe { read_html(clipboard.html, 64 * 1024) }.as_deref() == Some(html));
    }
}
