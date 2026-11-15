#![allow(unsafe_code)]

use std::{
    io,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::Source;
mod deferred;

use unikvm_core::geometry::Edge;
use windows::Win32::{
    Foundation::{
        DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND as OleWindow,
        POINTL, S_OK,
    },
    System::{
        Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL},
        Ole::{
            DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource,
            IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize,
            RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
        },
        SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
    },
    UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName},
};
use windows::core::{BOOL, HRESULT, HSTRING, Ref, Result, implement};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{COLOR_HIGHLIGHT, GetSysColorBrush},
        System::{LibraryLoader::GetModuleHandleW, Ole::CF_HDROP},
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            GetSystemMetrics, MSG, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN,
            SW_SHOWNOACTIVATE, SetTimer, ShowWindow, TranslateMessage, WM_CLOSE, WM_LBUTTONDOWN,
            WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
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

#[implement(IDropSource)]
struct FileSource(Arc<AtomicBool>);

#[allow(non_snake_case)]
impl IDropSource_Impl for FileSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if !keys.contains(MK_LBUTTON) {
            self.0.store(true, Ordering::Release);
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn file_object(path: &Path, _ole: &Apartment) -> Result<IDataObject> {
    // SAFETY: The caller keeps the OLE apartment alive while using this object.
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
        item.BindToHandler(None, &BHID_DataObject)
    }
}

pub fn run(edge: Edge, source: Option<Source<'_>>) -> io::Result<()> {
    let ole = Apartment::new()?;
    let released = Arc::new(AtomicBool::new(false));
    let object = match source {
        Some(Source::File(path)) => Some(file_object(path, &ole).map_err(io::Error::other)?),
        Some(Source::Deferred(path)) => Some(deferred::file_object(path, released.clone())?),
        None => None,
    };
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
        let _registration = if object.is_none() {
            let target: IDropTarget = EdgeTarget.into();
            RegisterDragDrop(OleWindow(window.0), &target).map_err(io::Error::other)?;
            Some(Registration(window.0))
        } else {
            None
        };
        if SetTimer(window.0, 1, 45_000, None) == 0 {
            return Err(io::Error::last_os_error());
        }
        if object.is_some() {
            println!("Press on the highlighted edge and drag into an empty Explorer folder.");
        } else {
            println!(
                "Drag an Explorer file to the highlighted edge and hold it. The probe closes after 45 seconds."
            );
        }
        ShowWindow(window.0, SW_SHOWNOACTIVATE);
        let mut event = MSG::default();
        loop {
            match GetMessageW(&mut event, null_mut(), 0, 0) {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(()),
                _ if event.hwnd == window.0 && event.message == WM_TIMER => return Ok(()),
                _ if event.hwnd == window.0 && event.message == WM_LBUTTONDOWN => {
                    if let Some(object) = &object {
                        let source: IDropSource = FileSource(released).into();
                        let mut effect = DROPEFFECT_NONE;
                        let result = DoDragDrop(object, &source, DROPEFFECT_COPY, &mut effect);
                        result.ok().map_err(io::Error::other)?;
                        super::report_drop(result == DRAGDROP_S_DROP && effect == DROPEFFECT_COPY);
                        return Ok(());
                    }
                }
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
        if message == WM_CLOSE {
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

    #[test]
    fn shell_explorer_data_object_exposes_regular_file_references() {
        let ole = Apartment::new().unwrap();
        let original = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let object = file_object(&original, &ole).unwrap();
        assert_eq!(references(&object).unwrap(), vec![original]);
    }
}
