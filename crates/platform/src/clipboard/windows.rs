#![allow(unsafe_code)]

use std::io;
use std::ptr::null_mut;

use image::ImageFormat;
use unikvm_core::clipboard::{Content, MAX_HTML_BYTES, MAX_PNG_BYTES, MAX_TEXT_BYTES};
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

use super::{Snapshot, WriteOutcome, cf_html, imaging};
use crate::input::windows::check_input_desktop;

pub struct Clipboard {
    exclude: u32,
    history: u32,
    cloud: u32,
    html: u32,
    png: u32,
}

impl Clipboard {
    pub fn new() -> io::Result<Self> {
        let exclude = format("ExcludeClipboardContentFromMonitorProcessing")?;
        let history = format("CanIncludeInClipboardHistory")?;
        let cloud = format("CanUploadToCloudClipboard")?;
        let html = format("HTML Format")?;
        let png = format("PNG")?;
        Ok(Self {
            exclude,
            history,
            cloud,
            html,
            png,
        })
    }

    pub fn available(&self) -> bool {
        check_input_desktop().is_ok()
    }

    pub fn sequence(&self) -> io::Result<u64> {
        // SAFETY: Reads the sequence number for the current window station.
        Ok(u64::from(unsafe { GetClipboardSequenceNumber() }))
    }

    pub fn read(&self, previous: u64) -> io::Result<Option<Snapshot>> {
        if self.sequence()? == previous {
            return Ok(None);
        }
        let clipboard = Open::new(null_mut())?;
        let sequence = self.sequence()?;
        if sequence == previous {
            return Ok(None);
        }
        // SAFETY: All formats are registered or standard, and the clipboard is open.
        let (mut content, image) = unsafe {
            if IsClipboardFormatAvailable(self.exclude) != 0
                || IsClipboardFormatAvailable(u32::from(CF_HDROP)) != 0
                || opt_out(self.history)
                || opt_out(self.cloud)
            {
                (None, None)
            } else {
                let text = if IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) != 0 {
                    read_text(MAX_TEXT_BYTES)?
                } else {
                    None
                };
                let image = if text.as_deref().is_none_or(str::is_empty) {
                    [self.png, u32::from(CF_DIBV5), u32::from(CF_DIB)]
                        .into_iter()
                        .find_map(|format| {
                            let limit = if format == self.png {
                                MAX_PNG_BYTES
                            } else {
                                imaging::MAX_NATIVE_BYTES
                            };
                            read_bytes(format, limit).map(|bytes| (format, bytes))
                        })
                } else {
                    None
                };
                let content = text.map(|plain| Content::Text {
                    plain,
                    html: read_html(self.html, MAX_HTML_BYTES),
                });
                (content, image)
            }
        };
        let sequence = self.sequence()?;
        drop(clipboard);
        if let Some((format, bytes)) = image {
            let png = if format == self.png {
                imaging::normalize(&bytes, ImageFormat::Png)
            } else {
                imaging::from_dib(&bytes)
            };
            if let Some(png) = png {
                content = Some(Content::Png(png));
            }
        }
        Ok((self.sequence()? == sequence).then_some(Snapshot { sequence, content }))
    }

    pub fn write(&self, expected: u64, content: &Content) -> io::Result<WriteOutcome> {
        let (format, data, extra) = match content {
            Content::Text { plain, html } => {
                let utf16: Vec<u16> = plain.encode_utf16().chain([0]).collect();
                let data = Memory::new(&utf16)?;
                let html = html
                    .as_ref()
                    .map(|text| {
                        Memory::new(cf_html::encode(text).as_bytes()).map(|data| (self.html, data))
                    })
                    .transpose()?;
                (u32::from(CF_UNICODETEXT), data, html)
            }
            Content::Png(png) => {
                let Some(pixels) = imaging::decode(png, ImageFormat::Png) else {
                    return Ok(WriteOutcome::Unsupported);
                };
                let Some(png) = imaging::to_png(&pixels) else {
                    return Ok(WriteOutcome::Unsupported);
                };
                let data = Memory::new(&imaging::to_dib(&pixels))?;
                let png = Memory::new(&png)?;
                (u32::from(CF_DIBV5), data, Some((self.png, png)))
            }
        };
        let excluded = Memory::new(&[0u8])?;
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
        if let Some((format, extra)) = extra {
            let _ = extra.publish(format);
        }
        data.publish(format)?;
        // Record the sequence under the clipboard lock. Windows may advance
        // it on close. The exclusion marker suppresses that notification
        // without also hiding a local copy that wins the race after close.
        Ok(WriteOutcome::Applied(self.sequence()?))
    }
}

unsafe fn read_bytes(format: u32, limit: usize) -> Option<Vec<u8>> {
    // SAFETY: The clipboard is open. Read only the allocated global block, then unlock.
    unsafe {
        if IsClipboardFormatAvailable(format) == 0 {
            return None;
        }
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let len = GlobalSize(handle);
        if len == 0 || len > limit {
            return None;
        }
        let data = GlobalLock(handle).cast::<u8>();
        if data.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data, len).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
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
        struct Restore<'a>(&'a Clipboard, bool, Option<Vec<u8>>);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                if let Ok(owner) = Owner::new()
                    && let Ok(_open) = Open::new(owner.0)
                {
                    // SAFETY: The clipboard is open. Clear only the marked test text.
                    unsafe {
                        if IsClipboardFormatAvailable(self.0.exclude) == 0
                            || !(read_text(64 * 1024).ok().flatten().as_deref() == Some(TEXT)
                                || self.2.as_ref().is_some_and(|test_png| {
                                    read_bytes(self.0.png, MAX_PNG_BYTES)
                                        .is_some_and(|bytes| bytes.starts_with(test_png))
                                }))
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
        let mut restore = Restore(&clipboard, had_empty_text, None);
        let html = "<strong>日本語🧪</strong>";
        let content = Content::Text {
            plain: TEXT.into(),
            html: Some(html.into()),
        };
        let WriteOutcome::Applied(_) = clipboard.write(empty, &content).unwrap() else {
            panic!("clipboard changed before the test write");
        };
        assert!(matches!(
            clipboard.write(empty, &content).unwrap(),
            WriteOutcome::Changed
        ));
        let snapshot = clipboard.read(empty).unwrap().unwrap();
        assert!(
            snapshot.content.is_none(),
            "remote text must not be reread for sync"
        );
        let open = Open::new(null_mut()).unwrap();
        // SAFETY: Read the test string directly from the currently open clipboard.
        assert!(unsafe { read_text(64 * 1024) }.unwrap().as_deref() == Some(TEXT));
        assert!(unsafe { read_html(clipboard.html, 64 * 1024) }.as_deref() == Some(html));
        drop(open);
        let pixels = image::RgbaImage::from_pixel(2, 3, image::Rgba([20, 80, 120, 64]));
        let png = imaging::to_png(&pixels).unwrap();
        restore.2 = Some(png.clone());
        let before_image = clipboard.sequence().unwrap();
        let result = clipboard
            .write(before_image, &Content::Png(png.clone()))
            .unwrap();
        assert!(matches!(result, WriteOutcome::Applied(_)));
        assert!(
            clipboard
                .read(before_image)
                .unwrap()
                .unwrap()
                .content
                .is_none()
        );
        let _open = Open::new(null_mut()).unwrap();
        // SAFETY: The clipboard is open on this thread.
        let native_png = unsafe { read_bytes(clipboard.png, MAX_PNG_BYTES) }.unwrap();
        assert!(native_png.starts_with(&png));
        let native_dib =
            unsafe { read_bytes(u32::from(CF_DIBV5), imaging::MAX_NATIVE_BYTES) }.unwrap();
        let decoded = imaging::from_dib(&native_dib).unwrap();
        assert_eq!(imaging::decode(&decoded, ImageFormat::Png).unwrap(), pixels);
    }
}
