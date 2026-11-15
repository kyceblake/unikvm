#![allow(unsafe_code)]

use std::{
    io,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod deferred;

use unikvm_core::geometry::Edge;
use windows::Win32::{
    Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK},
    System::{
        Com::IDataObject,
        Ole::{
            DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource,
            IDropSource_Impl, OleInitialize, OleUninitialize,
        },
        SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
    },
};
use windows::core::{BOOL, HRESULT, implement};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{COLOR_HIGHLIGHT, GetSysColorBrush},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            GetSystemMetrics, MSG, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN,
            SW_SHOWNOACTIVATE, SetTimer, ShowWindow, TranslateMessage, WM_CLOSE, WM_LBUTTONDOWN,
            WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
    core::w,
};

#[cfg(feature = "file-drag-probe")]
use {
    crate::drag_probe::{self, Source},
    std::path::Path,
    windows::{
        Win32::{
            Foundation::{HWND as OleWindow, POINTL},
            System::{
                Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL},
                Ole::{
                    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium,
                    RevokeDragDrop,
                },
            },
            UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName},
        },
        core::{HSTRING, Ref, Result},
    },
    windows_sys::Win32::System::Ole::CF_HDROP,
};

#[cfg(feature = "file-drag-probe")]
#[implement(IDropTarget)]
struct EdgeTarget;

#[cfg(feature = "file-drag-probe")]
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
        drag_probe::report(object.as_ref().and_then(references));
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

#[cfg(feature = "file-drag-probe")]
fn reject(effect: *mut DROPEFFECT) {
    // SAFETY: OLE provides a writable effect pointer for the callback.
    if let Some(effect) = unsafe { effect.as_mut() } {
        *effect = DROPEFFECT_NONE;
    }
}

#[cfg(feature = "file-drag-probe")]
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

#[cfg(feature = "file-drag-probe")]
fn file_object(path: &Path, _ole: &Apartment) -> Result<IDataObject> {
    // SAFETY: The caller keeps the OLE apartment alive while using this object.
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
        item.BindToHandler(None, &BHID_DataObject)
    }
}

#[cfg(feature = "file-drag-probe")]
pub(crate) fn run_probe(edge: Edge, source: Option<Source<'_>>) -> io::Result<()> {
    let ole = Apartment::new()?;
    let released = Arc::new(AtomicBool::new(false));
    let object = match source {
        Some(Source::File(path)) => Some(file_object(path, &ole).map_err(io::Error::other)?),
        Some(Source::Deferred(path)) => Some(deferred::file_object(path, released.clone())?),
        None => None,
    };
    let copied = run_edge(edge, object, released)?;
    if let Some(copied) = copied {
        drag_probe::report_drop(copied);
    }
    Ok(())
}

/// Advertise the remote file until a native target requests its verified local copy.
pub fn run_received(
    edge: Edge,
    name: &str,
    size: u64,
    verified_path: impl Fn() -> io::Result<PathBuf> + Send + Sync + 'static,
) -> io::Result<bool> {
    let _ole = Apartment::new()?;
    let released = Arc::new(AtomicBool::new(false));
    let object = deferred::received_object(name, size, released.clone(), verified_path)?;
    run_edge(edge, Some(object), released).map(|copied| copied.unwrap_or(false))
}

fn run_edge(
    edge: Edge,
    object: Option<IDataObject>,
    released: Arc<AtomicBool>,
) -> io::Result<Option<bool>> {
    // SAFETY: This thread owns the window, registration and message queue.
    unsafe {
        let width = 18;
        let left = match edge {
            Edge::Left => 0,
            Edge::Right => GetSystemMetrics(SM_CXSCREEN) - width,
            _ => return Err(io::Error::other("choose a horizontal edge")),
        };
        let application = GetModuleHandleW(null());
        let name = w!("UniKVMFileDrag");
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
        #[cfg(feature = "file-drag-probe")]
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
                0 => return Ok(None),
                _ if event.hwnd == window.0 && event.message == WM_TIMER => return Ok(None),
                _ if event.hwnd == window.0 && event.message == WM_LBUTTONDOWN => {
                    if let Some(object) = &object {
                        let source: IDropSource = FileSource(released).into();
                        let mut effect = DROPEFFECT_NONE;
                        let result = DoDragDrop(object, &source, DROPEFFECT_COPY, &mut effect);
                        result.ok().map_err(io::Error::other)?;
                        return Ok(Some(result == DRAGDROP_S_DROP && effect == DROPEFFECT_COPY));
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

#[cfg(feature = "file-drag-probe")]
struct Registration(HWND);

#[cfg(feature = "file-drag-probe")]
impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: Revoke the registered drop target before destroying the window.
        let _ = unsafe { RevokeDragDrop(OleWindow(self.0)) };
    }
}

#[cfg(all(test, feature = "file-drag-probe"))]
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
