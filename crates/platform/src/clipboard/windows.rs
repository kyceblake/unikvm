#![allow(unsafe_code)]

use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::null_mut;
use std::{fs, io};

use image::ImageFormat;
use unikvm_core::clipboard::{Content, MAX_FILES, MAX_HTML_BYTES, MAX_PNG_BYTES, MAX_TEXT_BYTES};
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

use super::{Snapshot, WriteOutcome, cf_html, imaging};
use crate::input::windows::check_input_desktop;

pub struct Clipboard {
    exclude: u32,
    imported: u32,
    history: u32,
    cloud: u32,
    html: u32,
    png: u32,
    drop_effect: u32,
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
            imported: format("UniKVM Received Clipboard")?,
            history,
            cloud,
            html,
            png,
            drop_effect: format("Preferred DropEffect")?,
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
        let (mut content, image, imported) = unsafe {
            let imported = IsClipboardFormatAvailable(self.imported) != 0;
            if IsClipboardFormatAvailable(self.exclude) != 0
                || imported
                || opt_out(self.history)
                || opt_out(self.cloud)
            {
                (None, None, imported)
            } else {
                let has_files = IsClipboardFormatAvailable(u32::from(CF_HDROP)) != 0;
                let text =
                    if !has_files && IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) != 0 {
                        read_text(MAX_TEXT_BYTES)
                    } else {
                        Ok(None)
                    };
                let image = if text
                    .as_ref()
                    .is_ok_and(|text| text.as_deref().is_none_or(str::is_empty))
                {
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
                let content = if has_files {
                    read_files().map(Content::Files)
                } else {
                    match text {
                        Ok(Some(plain)) => match read_html(self.html, MAX_HTML_BYTES) {
                            Ok(html) => Some(Content::Text { plain, html }),
                            Err(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                                Some(Content::TooLarge)
                            }
                            Err(error) => return Err(error),
                        },
                        Ok(None) => None,
                        Err(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                            Some(Content::TooLarge)
                        }
                        Err(error) => return Err(error),
                    }
                };
                (content, image, imported)
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
        Ok((self.sequence()? == sequence).then_some(Snapshot {
            sequence,
            content,
            imported,
        }))
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
            Content::Files(paths) => (
                u32::from(CF_HDROP),
                file_drop(paths)?,
                Some((self.drop_effect, Memory::new(&[1u32])?)),
            ),
            Content::TooLarge => return Ok(WriteOutcome::Unsupported),
        };
        let excluded = Memory::new(&[0u8])?;
        let imported = Memory::new(&[0u8])?;
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
        imported.publish(self.imported)?;
        if let Some((format, extra)) = extra {
            let _ = extra.publish(format);
        }
        data.publish(format)?;
        // Record the sequence under the clipboard lock. Windows may advance
        // it on close. The exclusion marker suppresses that notification
        // without also hiding a local copy that wins the race after close.
        Ok(WriteOutcome::Applied(self.sequence()?))
    }

    pub fn imported_files(&self) -> io::Result<Option<Vec<PathBuf>>> {
        let _clipboard = Open::new(null_mut())?;
        // SAFETY: The clipboard lock keeps both representations live.
        Ok(unsafe {
            (IsClipboardFormatAvailable(self.imported) != 0)
                .then(|| read_files())
                .flatten()
        })
    }
}

pub fn staging_directory() -> io::Result<PathBuf> {
    let mut session = 0;
    // SAFETY: The process ID is current and the session output is writable.
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user_dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|directory| directory.is_absolute())
        .ok_or_else(|| io::Error::other("Windows user data directory is unavailable"))?;
    let directory = user_dir
        .join("Temp")
        .join(format!("UniKVM-clipboard-{session}"));
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(directory)
}

pub fn open_source(path: &Path) -> io::Result<fs::File> {
    if !path.is_absolute()
        || !matches!(path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(file)
}

unsafe fn read_files() -> Option<Vec<PathBuf>> {
    // SAFETY: The clipboard owns the HDROP until the lock is released.
    unsafe { read_drop(GetClipboardData(u32::from(CF_HDROP))) }
}

pub(crate) unsafe fn read_drop(drop: HGLOBAL) -> Option<Vec<PathBuf>> {
    // SAFETY: The caller supplies a live HDROP global memory block or null.
    unsafe {
        if drop.is_null() {
            return None;
        }
        let count = DragQueryFileW(drop, u32::MAX, null_mut(), 0);
        if !(1..=MAX_FILES as u32).contains(&count) {
            return None;
        }
        (0..count)
            .map(|index| {
                let length = DragQueryFileW(drop, index, null_mut(), 0);
                if !(1..=32_767).contains(&length) {
                    return None;
                }
                let mut buffer = vec![0; length as usize + 1];
                if DragQueryFileW(drop, index, buffer.as_mut_ptr(), length + 1) != length {
                    return None;
                }
                Some(PathBuf::from(std::ffi::OsString::from_wide(
                    &buffer[..length as usize],
                )))
            })
            .collect()
    }
}

fn file_drop(paths: &[PathBuf]) -> io::Result<Memory> {
    // DROPFILES contains five DWORDs. Keep the offsets independent of pointer size.
    let mut data = Vec::new();
    for field in [size_of::<DROPFILES>() as u32, 0, 0, 0, 1] {
        data.extend_from_slice(&field.to_le_bytes());
    }
    for path in paths {
        for unit in path.as_os_str().encode_wide().chain([0]) {
            data.extend_from_slice(&unit.to_le_bytes());
        }
    }
    data.extend_from_slice(&[0, 0]);
    Memory::new(&data)
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

unsafe fn read_html(format: u32, limit: usize) -> io::Result<Option<String>> {
    // SAFETY: The caller holds the clipboard lock. GlobalSize bounds the
    // readable bytes, and the returned string owns its data before unlock.
    unsafe {
        if IsClipboardFormatAvailable(format) == 0 {
            return Ok(None);
        }
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return Ok(None);
        }
        let bytes = GlobalSize(handle);
        if bytes > limit.saturating_add(cf_html::MAX_HEADER_BYTES) {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        let data = GlobalLock(handle).cast::<u8>();
        if data.is_null() {
            return Ok(None);
        }
        let fragment = cf_html::decode(std::slice::from_raw_parts(data, bytes), bytes)
            .map(|fragment| {
                if fragment.len() > limit {
                    Err(io::ErrorKind::FileTooLarge.into())
                } else {
                    Ok(fragment.to_owned())
                }
            })
            .transpose();
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
        let length = GlobalSize(handle) / size_of::<u16>();
        let units = length.min(limit.saturating_add(1));
        let data = GlobalLock(handle).cast::<u16>();
        if data.is_null() {
            return Err(io::Error::last_os_error());
        }
        let units = std::slice::from_raw_parts(data, units);
        let end = units.iter().position(|&unit| unit == 0);
        let text = end.and_then(|end| String::from_utf16(&units[..end]).ok());
        GlobalUnlock(handle);
        if (end.is_none() && length > limit) || text.as_ref().is_some_and(|text| text.len() > limit)
        {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
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
    fn native_file_drop_preserves_multiple_unicode_paths() {
        let paths = vec![
            PathBuf::from(r"C:\Clipboard\写真.png"),
            PathBuf::from(r"C:\Clipboard\report.test"),
        ];
        let block = file_drop(&paths).unwrap();
        // SAFETY: The prepared DROPFILES global memory block remains live.
        assert_eq!(unsafe { read_drop(block.0) }.unwrap(), paths);
    }

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
        assert!(
            unsafe { read_html(clipboard.html, 64 * 1024) }
                .unwrap()
                .as_deref()
                == Some(html)
        );
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
