#![allow(unsafe_code)]

use std::{
    io,
    path::PathBuf,
    ptr::{null, null_mut},
};

use unikvm_core::geometry::Edge;
use windows::Win32::{
    Foundation::{HWND as OleWindow, POINTL},
    System::{
        Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL},
        Ole::{
            DROPEFFECT, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, OleInitialize,
            OleUninitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
        },
        SystemServices::MODIFIERKEYS_FLAGS,
    },
};
use windows::core::{Ref, Result, implement};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{COLOR_HIGHLIGHT, GetSysColorBrush},
        System::{LibraryLoader::GetModuleHandleW, Ole::CF_HDROP},
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            GetSystemMetrics, MSG, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN,
            SW_SHOWNOACTIVATE, SetTimer, ShowWindow, TranslateMessage, WM_CLOSE, WM_TIMER,
            WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
    core::w,
};

#[implement(IDropTarget)]
struct EdgeTarget;

#[allow(non_snake_case)]
impl IDropTarget_Impl for EdgeTarget_Impl {
    fn DragEnter(
        &self,
        object: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        reject(effect);
        super::report(object.as_ref().and_then(references));
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        _point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        reject(effect);
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        _object: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        reject(effect);
        Ok(())
    }
}

fn reject(effect: *mut DROPEFFECT) {
    // SAFETY: OLE provides a writable effect pointer for the callback.
    if let Some(effect) = unsafe { effect.as_mut() } {
        *effect = DROPEFFECT_NONE;
    }
}

fn references(object: &IDataObject) -> Option<Vec<PathBuf>> {
    let format = FORMATETC {
        cfFormat: CF_HDROP,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    };
    // SAFETY: The live data object runs on this OLE apartment. Read only the
    // shell's file references. Release the data object's medium exactly once.
    unsafe {
        if object.QueryGetData(&format).is_err() {
            return None;
        }
        let mut medium = object.GetData(&format).ok()?;
        let paths = (medium.tymed == TYMED_HGLOBAL.0 as u32)
            .then(|| crate::clipboard::read_drop(medium.u.hGlobal.0))
            .flatten();
        ReleaseStgMedium(&mut medium);
        paths
    }
}

pub fn run(edge: Edge) -> io::Result<()> {
    let _ole = Apartment::new()?;
    // SAFETY: This thread owns the window, registration and message queue.
    unsafe {
        let width = 18;
        let left = match edge {
            Edge::Left => 0,
            Edge::Right => GetSystemMetrics(SM_CXSCREEN) - width,
            _ => return Err(io::Error::other("choose a horizontal edge")),
        };
        let application = GetModuleHandleW(null());
        let name = w!("UniKVMFileDragProbe");
        if RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: application,
            hbrBackground: GetSysColorBrush(COLOR_HIGHLIGHT),
            lpszClassName: name,
            ..Default::default()
        }) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            name,
            name,
            WS_POPUP,
            left,
            0,
            width,
            GetSystemMetrics(SM_CYSCREEN),
            null_mut(),
            null_mut(),
            application,
            null(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }
        let window = Window(hwnd);
        let target: IDropTarget = EdgeTarget.into();
        RegisterDragDrop(OleWindow(window.0), &target).map_err(io::Error::other)?;
        let _registration = Registration(window.0);
        if SetTimer(window.0, 1, 45_000, None) == 0 {
            return Err(io::Error::last_os_error());
        }
        println!(
            "Drag an Explorer file to the highlighted edge and hold it. The probe closes after 45 seconds."
        );
        ShowWindow(window.0, SW_SHOWNOACTIVATE);
        let mut event = MSG::default();
        loop {
            match GetMessageW(&mut event, null_mut(), 0, 0) {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(()),
                _ => {
                    TranslateMessage(&event);
                    DispatchMessageW(&event);
                }
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    // SAFETY: Win32 supplies this window and message. Cleanup is run by the UI thread.
    unsafe {
        if matches!(message, WM_CLOSE | WM_TIMER) {
            PostQuitMessage(0);
            0
        } else {
            DefWindowProcW(window, message, wp, lp)
        }
    }
}

struct Apartment;

impl Apartment {
    fn new() -> io::Result<Self> {
        // SAFETY: Initialization is balanced by Drop on this thread.
        unsafe { OleInitialize(None) }.map_err(io::Error::other)?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: This thread owns the OLE apartment.
        unsafe { OleUninitialize() };
    }
}

struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: The window belongs to the calling thread.
        unsafe { DestroyWindow(self.0) };
    }
}

struct Registration(HWND);

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: Revoke the registered drop target before destroying the window.
        let _ = unsafe { RevokeDragDrop(OleWindow(self.0)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName},
        core::HSTRING,
    };

    #[test]
    fn shell_explorer_data_object_exposes_regular_file_references() {
        let _ole = Apartment::new().unwrap();
        let original = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        // SAFETY: The Shell item and data object remain in this OLE apartment.
        let object: IDataObject = unsafe {
            let item: IShellItem =
                SHCreateItemFromParsingName(&HSTRING::from(original.as_os_str()), None).unwrap();
            item.BindToHandler(None, &BHID_DataObject).unwrap()
        };
        assert_eq!(references(&object).unwrap(), vec![original]);
    }
}
