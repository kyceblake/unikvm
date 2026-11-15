#![allow(unsafe_code)]

use std::{
    io,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

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
};
use windows::core::{BOOL, HRESULT, Ref, Result, implement};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{COLOR_HIGHLIGHT, GetSysColorBrush},
        System::{LibraryLoader::GetModuleHandleW, Ole::CF_HDROP},
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetLayeredWindowAttributes, GetMessageW, GetSystemMetrics, LWA_ALPHA, MSG,
                PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE,
                SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage, WM_CLOSE,
                WM_LBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WindowFromPoint,
            },
        },
    },
    core::w,
};

const EXPIRE_TIMER: usize = 1;
const HOVER_TIMER: usize = 2;
// Win32 makes zero-alpha windows click-through, including for the injected press.
const IDLE_ALPHA: u8 = 1;
const HOVER_ALPHA: u8 = 64;

#[cfg(feature = "file-drag-probe")]
use {
    crate::drag_probe::{self, Source},
    std::path::Path,
    windows::{
        Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName},
        core::HSTRING,
    },
};

#[implement(IDropTarget)]
struct EdgeTarget {
    report: Box<dyn Fn(Option<Vec<PathBuf>>)>,
    require_primary: bool,
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for EdgeTarget_Impl {
    fn DragEnter(
        &self,
        object: Ref<'_, IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        _point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        reject(effect);
        if !self.require_primary || keys == MK_LBUTTON {
            (self.report)(object.as_ref().and_then(references));
        }
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
struct FileSource {
    released: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

#[allow(non_snake_case)]
impl IDropSource_Impl for FileSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool()
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            DRAGDROP_S_CANCEL
        } else if !keys.contains(MK_LBUTTON) {
            self.released.store(true, Ordering::Release);
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
    let copied = run_edge(edge, object, released, false, || Ok(()))?;
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
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<bool> {
    let _ole = Apartment::new()?;
    let released = Arc::new(AtomicBool::new(false));
    let object = deferred::received_object(name, size, released.clone(), verified_path)?;
    run_edge(edge, Some(object), released, false, ready).map(|copied| copied.unwrap_or(false))
}

/// Resume a remote Finder drag with this desktop's physical mouse.
/// The caller restores capture and posts the tagged down before authorizing readiness.
pub fn run_received_return(
    edge: Edge,
    name: &str,
    size: u64,
    verified_path: impl Fn() -> io::Result<PathBuf> + Send + Sync + 'static,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<bool> {
    let _ole = Apartment::new()?;
    let released = Arc::new(AtomicBool::new(false));
    let object = deferred::received_object(name, size, released.clone(), verified_path)?;
    run_edge(edge, Some(object), released, true, ready).map(|copied| copied.unwrap_or(false))
}

struct ReturnedButton(Arc<AtomicBool>);

impl Drop for ReturnedButton {
    fn drop(&mut self) {
        if !self.0.load(Ordering::Acquire) {
            let _ = returned_button(MOUSEEVENTF_LEFTUP);
        }
    }
}

fn returned_button(flags: u32) -> io::Result<()> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: flags,
                dwExtraInfo: crate::input::EVENT_TAG,
                ..Default::default()
            },
        },
    };
    // SAFETY: INPUT is fully initialized; its tag bypasses UniKVM's capture hook.
    if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } != 1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Observe a one-file Explorer drag without accepting the local drop.
pub fn observe(edge: Edge, file: impl Fn(PathBuf) + 'static) -> io::Result<()> {
    let _ole = Apartment::new()?;
    // SAFETY: These calls act on the OLE thread and its owned edge window.
    unsafe {
        if SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_null() {
            return Err(io::Error::last_os_error());
        }
        // The receiving pointer lands two pixels inside the edge, past this strip.
        let window = edge_window(edge, 2)?;
        let _registration = Registration::new(
            &window,
            EdgeTarget {
                require_primary: true,
                report: Box::new(move |paths| {
                    if let Some([path]) = paths.as_deref() {
                        file(path.clone());
                    }
                }),
            },
        )?;
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

fn run_edge(
    edge: Edge,
    object: Option<IDataObject>,
    released: Arc<AtomicBool>,
    local_return: bool,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<Option<bool>> {
    // SAFETY: This thread owns the window, registration and message queue.
    unsafe {
        let window = edge_window(edge, 18)?;
        #[cfg(feature = "file-drag-probe")]
        let _registration = if object.is_none() {
            Some(Registration::new(
                &window,
                EdgeTarget {
                    report: Box::new(drag_probe::report),
                    require_primary: false,
                },
            )?)
        } else {
            None
        };
        if SetTimer(window.0, EXPIRE_TIMER, 45_000, None) == 0 {
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
        // The capture owner posts the tagged down while restoring local input.
        // Balance it even if activation fails, the drag is cancelled, or OLE errors.
        let _returned_button = local_return.then(|| ReturnedButton(released.clone()));
        ready()?;
        let mut event = MSG::default();
        loop {
            match GetMessageW(&mut event, null_mut(), 0, 0) {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(None),
                _ if event.hwnd == window.0
                    && event.message == WM_TIMER
                    && event.wParam == EXPIRE_TIMER =>
                {
                    return Ok(None);
                }
                _ if event.hwnd == window.0 && event.message == WM_LBUTTONDOWN => {
                    if let Some(object) = &object {
                        let source: IDropSource = FileSource {
                            released,
                            deadline: local_return
                                .then(|| Instant::now() + Duration::from_secs(45)),
                        }
                        .into();
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

fn edge_window(edge: Edge, width: i32) -> io::Result<Window> {
    // SAFETY: This thread owns the class callback and the new window.
    unsafe {
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
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
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
        if SetLayeredWindowAttributes(hwnd, 0, IDLE_ALPHA, LWA_ALPHA) == 0
            || SetTimer(hwnd, HOVER_TIMER, 40, None) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(window)
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
        } else if message == WM_TIMER && wp == HOVER_TIMER {
            let mut pointer = POINT::default();
            let hovered = GetCursorPos(&mut pointer) != 0 && WindowFromPoint(pointer) == window;
            let alpha = if hovered { HOVER_ALPHA } else { IDLE_ALPHA };
            let mut current = 0;
            if GetLayeredWindowAttributes(window, null_mut(), &mut current, null_mut()) != 0
                && current != alpha
            {
                SetLayeredWindowAttributes(window, 0, alpha, LWA_ALPHA);
            }
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

impl Registration {
    fn new(window: &Window, target: EdgeTarget) -> io::Result<Self> {
        let target: IDropTarget = target.into();
        // SAFETY: The window and target share the initialized OLE apartment.
        unsafe { RegisterDragDrop(OleWindow(window.0), &target) }.map_err(io::Error::other)?;
        Ok(Self(window.0))
    }
}

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
    #[ignore = "requires an interactive Windows desktop with a stationary pointer"]
    fn idle_edge_receives_hits_and_highlights_only_while_hovered() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_TOPMOST, SWP_NOACTIVATE, SetWindowPos,
        };

        let window = edge_window(Edge::Left, 18).unwrap();
        let mut pointer = POINT::default();
        // SAFETY: The test moves only its own small overlay. All Win32 outputs
        // point to live storage and the window remains on this thread.
        unsafe {
            assert_ne!(GetCursorPos(&mut pointer), 0);
            let move_overlay = |x_offset| {
                assert_ne!(
                    SetWindowPos(
                        window.0,
                        HWND_TOPMOST,
                        pointer.x - 4 + x_offset,
                        pointer.y - 4,
                        9,
                        9,
                        SWP_NOACTIVATE,
                    ),
                    0
                );
            };
            let alpha = || {
                let mut value = 0;
                assert_ne!(
                    GetLayeredWindowAttributes(window.0, null_mut(), &mut value, null_mut()),
                    0
                );
                value
            };
            move_overlay(0);
            ShowWindow(window.0, SW_SHOWNOACTIVATE);
            assert_eq!(alpha(), IDLE_ALPHA);
            assert_eq!(WindowFromPoint(pointer), window.0);
            window_proc(window.0, WM_TIMER, HOVER_TIMER, 0);
            assert_eq!(alpha(), HOVER_ALPHA);
            move_overlay(30);
            window_proc(window.0, WM_TIMER, HOVER_TIMER, 0);
            assert_eq!(alpha(), IDLE_ALPHA);
        }
    }

    #[test]
    fn shell_explorer_data_object_exposes_regular_file_references() {
        let ole = Apartment::new().unwrap();
        let original = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let object = file_object(&original, &ole).unwrap();
        assert_eq!(references(&object).unwrap(), vec![original]);
    }

    #[test]
    fn explorer_source_reads_only_the_primary_drag_and_declines_the_local_drop() {
        use std::{cell::RefCell, rc::Rc};
        use windows::Win32::System::SystemServices::{MK_CONTROL, MK_RBUTTON};

        let ole = Apartment::new().unwrap();
        let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let object = file_object(&original, &ole).unwrap();
        let observed = Rc::new(RefCell::new(Vec::new()));
        let target: IDropTarget = EdgeTarget {
            require_primary: true,
            report: Box::new({
                let observed = observed.clone();
                move |paths| observed.borrow_mut().push(paths.unwrap())
            }),
        }
        .into();
        // SAFETY: The Shell file and drop interfaces remain on this OLE thread.
        unsafe {
            for keys in [MK_RBUTTON, MK_LBUTTON | MK_CONTROL, MK_LBUTTON] {
                let mut effect = DROPEFFECT_COPY;
                target
                    .DragEnter(&object, keys, POINTL::default(), &mut effect)
                    .unwrap();
                assert_eq!(effect, DROPEFFECT_NONE);
            }
            let mut effect = DROPEFFECT_COPY;
            target
                .Drop(
                    &object,
                    MODIFIERKEYS_FLAGS(0),
                    POINTL::default(),
                    &mut effect,
                )
                .unwrap();
            assert_eq!(effect, DROPEFFECT_NONE);
        }
        assert_eq!(*observed.borrow(), vec![vec![original]]);
    }
}
