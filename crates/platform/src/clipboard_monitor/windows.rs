#![allow(unsafe_code)]

use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::{
    io,
    mem::size_of,
    ptr::{null, null_mut},
};
use unikvm_protocol::tray::{Command, Connection, Phase, Transfer, View};
use windows::Win32::{
    Foundation::HWND as ComWindow,
    System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    },
    UI::Shell::{
        CLSID_ProgressDialog, IOperationsProgressDialog, IProgressDialog,
        OPPROGDLG_DONTDISPLAYLOCATIONS, PROGDLG_AUTOTIME, PropertiesSystem::PDOPS_CANCELLED,
        SPACTION_COPYING,
    },
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, FreeLibrary, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM,
    },
    System::{
        LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW},
        Threading::CreateMutexW,
    },
    UI::{
        Controls::{TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDF_ALLOW_DIALOG_CANCELLATION},
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW,
            GetWindowLongPtrW, IDI_APPLICATION, LoadIconW, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG,
            PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
            SetForegroundWindow, SetTimer, SetWindowLongPtrW, TPM_NONOTIFY, TPM_RETURNCMD,
            TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_DESTROY,
            WM_NULL, WM_TIMER, WNDCLASSW,
        },
    },
};
use windows_sys::core::w;

use super::{
    Snapshot, TransferDisplay, UI_POLL_MS, clipboard_status, connection, peer_label, tooltip,
};

const CALLBACK: u32 = WM_APP + 1;
const OPEN_MENU: u32 = WM_APP + 2;
const RESTORE_ICON: u32 = WM_APP + 3;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const TIMER: usize = 1;
const CONNECT: u32 = 10;
const DISCONNECT: u32 = 11;
const EXIT: u32 = 12;

pub(super) fn show(read: impl Fn() -> Snapshot, send: impl Fn(Command)) -> io::Result<()> {
    // SAFETY: The tray owns a window, mutex and COM apartment for this UI thread.
    unsafe {
        let name = wide(&crate::local::Endpoint::for_current_user()?.tray_name());
        let mutex = CreateMutexW(null(), 0, name.as_ptr());
        let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
        if mutex.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _lock = OwnedHandle::from_raw_handle(mutex);
        if already_exists {
            return Ok(());
        }
        let _com = Apartment::new()?;
        let instance = GetModuleHandleW(null());
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        let class = w!("UniKVMTrayWindow");
        if RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        }) == 0
            || taskbar_created == 0
        {
            return Err(io::Error::last_os_error());
        }
        // A hidden top-level window receives Explorer's TaskbarCreated broadcast.
        let window = CreateWindowExW(
            0,
            class,
            w!("UniKVM"),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        SetWindowLongPtrW(window, GWLP_USERDATA, taskbar_created as isize);
        let mut icon = TrayIcon(NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: CALLBACK,
            hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
            ..Default::default()
        });
        let mut tips = tooltip(read().view.as_ref(), 127);
        icon.set_tip(&tips);
        icon.add();
        if SetTimer(window, TIMER, UI_POLL_MS as u32, None) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut progress: [Progress; 2] = std::array::from_fn(|_| Progress::new());
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, null_mut(), 0, 0) {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(()),
                _ => {}
            }
            match message.message {
                RESTORE_ICON if message.hwnd == window => icon.add(),
                WM_TIMER if message.hwnd == window && message.wParam == TIMER => {
                    let snapshot = read();
                    if snapshot.close {
                        return Ok(());
                    }
                    let next = tooltip(snapshot.view.as_ref(), 127);
                    if next != tips {
                        icon.set_tip(&next);
                        Shell_NotifyIconW(NIM_MODIFY, &icon.0);
                        tips = next;
                    }
                    for (index, indicator) in progress.iter_mut().enumerate() {
                        indicator.update(
                            snapshot
                                .view
                                .as_ref()
                                .and_then(|view| view.transfers[index].as_ref()),
                            index,
                            &send,
                        );
                    }
                }
                OPEN_MENU if message.hwnd == window => {
                    if let Some(command) = popup(window, read().view.as_ref()) {
                        send(command);
                    }
                }
                _ => {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

struct TrayIcon(NOTIFYICONDATAW);

impl TrayIcon {
    fn add(&mut self) {
        // SAFETY: The local structure and its window are live on the UI thread.
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &self.0) != 0 {
                self.0.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &self.0);
            }
        }
    }

    fn set_tip(&mut self, tip: &str) {
        self.0.szTip.fill(0);
        for (slot, code) in self.0.szTip.iter_mut().take(127).zip(tip.encode_utf16()) {
            *slot = code;
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // SAFETY: This owns the notification icon and its hidden window.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.0);
            DestroyWindow(self.0.hWnd);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    // SAFETY: The system supplied this window and message.
    unsafe {
        let taskbar_created = GetWindowLongPtrW(window, GWLP_USERDATA) as u32;
        if taskbar_created != 0 && message == taskbar_created {
            PostMessageW(window, RESTORE_ICON, 0, 0);
            0
        } else if message == CALLBACK {
            let event = lp as u32 & 0xffff;
            if matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT) {
                PostMessageW(window, OPEN_MENU, 0, 0);
            }
            0
        } else if message == WM_DESTROY {
            PostQuitMessage(0);
            0
        } else {
            DefWindowProcW(window, message, wp, lp)
        }
    }
}

unsafe fn popup(window: HWND, view: Option<&View>) -> Option<Command> {
    // SAFETY: The menu is synchronous and all input strings live through AppendMenuW.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        let add = |id, flags, label: &str| {
            let text = wide(&label.replace('&', "&&"));
            AppendMenuW(menu, flags | MF_STRING, id as usize, text.as_ptr());
        };
        add(0, MF_GRAYED, connection(view));
        if let Some(view) = view {
            for peer in &view.peers {
                add(0, MF_GRAYED, &peer_label(peer));
            }
            for status in (0..2).filter_map(|direction| clipboard_status(view, direction)) {
                add(0, MF_GRAYED, &status);
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            match view.connection {
                Connection::Disconnected => add(CONNECT, 0, "Connect"),
                Connection::Online => add(DISCONNECT, 0, "Disconnect"),
                _ => {}
            }
        }
        add(EXIT, 0, "Exit completely");
        let mut position = POINT::default();
        GetCursorPos(&mut position);
        SetForegroundWindow(window);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            position.x,
            position.y,
            0,
            window,
            null(),
        );
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        match selected as u32 {
            CONNECT => Some(Command::Connect),
            DISCONNECT => Some(Command::Disconnect),
            EXIT => Some(Command::Exit),
            _ => None,
        }
    }
}

struct Apartment;

impl Apartment {
    fn new() -> io::Result<Self> {
        // SAFETY: Balanced by Drop on the same main thread.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(io::Error::other)?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: The UI thread initialized this apartment.
        unsafe { CoUninitialize() };
    }
}

struct Progress {
    display: TransferDisplay,
    dialog: Option<FileDialog>,
}

enum FileDialog {
    Operations(IOperationsProgressDialog),
    Standard(IProgressDialog),
}

impl FileDialog {
    fn open(item: &Transfer, direction: usize) -> windows::core::Result<Self> {
        // SAFETY: This runs on the tray's COM apartment, with parameters live through each call.
        unsafe {
            if let Ok(dialog) = CoCreateInstance::<_, IOperationsProgressDialog>(
                &CLSID_ProgressDialog,
                None,
                CLSCTX_INPROC_SERVER,
            ) && dialog
                .StartProgressDialog(
                    ComWindow::default(),
                    OPPROGDLG_DONTDISPLAYLOCATIONS.0 as u32,
                )
                .is_ok()
            {
                let _ = dialog.SetOperation(SPACTION_COPYING);
                return Ok(Self::Operations(dialog));
            }
            // Some desktop versions expose only IProgressDialog on this class.
            let dialog: IProgressDialog =
                CoCreateInstance(&CLSID_ProgressDialog, None, CLSCTX_INPROC_SERVER)?;
            let action = if direction == 0 {
                "Sending to"
            } else {
                "Receiving from"
            };
            dialog.SetTitle(windows::core::w!("UniKVM file transfer"))?;
            dialog.SetLine(
                1,
                &windows::core::HSTRING::from(format!("{action} {}", item.peer)),
                false,
                None,
            )?;
            dialog.SetLine(2, &windows::core::HSTRING::from(&item.label), false, None)?;
            dialog.StartProgressDialog(None, None, PROGDLG_AUTOTIME, None)?;
            Ok(Self::Standard(dialog))
        }
    }

    fn update(&self, item: &Transfer) -> bool {
        let total = item.total.max(1);
        // SAFETY: The COM object belongs to this apartment and has started.
        unsafe {
            match self {
                Self::Operations(dialog) => {
                    let _ = dialog.UpdateProgress(
                        item.bytes,
                        total,
                        item.bytes,
                        item.total,
                        0,
                        u64::from(item.items),
                    );
                    dialog
                        .GetOperationStatus()
                        .is_ok_and(|status| status == PDOPS_CANCELLED)
                }
                Self::Standard(dialog) => {
                    let _ = dialog.SetProgress64(item.bytes, total);
                    dialog.HasUserCancelled().as_bool()
                }
            }
        }
    }
}

impl Drop for FileDialog {
    fn drop(&mut self) {
        // SAFETY: Stop before releasing the object on its owning apartment.
        unsafe {
            let _ = match self {
                Self::Operations(dialog) => dialog.StopProgressDialog(),
                Self::Standard(dialog) => dialog.StopProgressDialog(),
            };
        }
    }
}

impl Progress {
    fn new() -> Self {
        Self {
            display: TransferDisplay::new(),
            dialog: None,
        }
    }

    fn update(&mut self, transfer: Option<&Transfer>, direction: usize, send: &dyn Fn(Command)) {
        let changed = self
            .display
            .observe(transfer)
            .is_some_and(|(_, changed)| changed);
        let Some(item) = transfer.filter(|item| item.phase == Phase::Copying) else {
            self.dialog = None;
            return;
        };
        if changed {
            self.dialog = FileDialog::open(item, direction).ok();
        }
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.update(item))
        {
            self.display.cancel(send);
            self.dialog = None;
        }
    }
}

pub(super) fn approve(item: &str, size: &str) -> io::Result<bool> {
    const DOWNLOAD: i32 = 100;
    const DECLINE: i32 = 101;
    let details = wide(&format!("Your paired computer offered {item} ({size})."));
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: DECLINE,
            pszButtonText: w!("Decline"),
        },
        TASKDIALOG_BUTTON {
            nButtonID: DOWNLOAD,
            pszButtonText: w!("Download"),
        },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        pszWindowTitle: w!("UniKVM"),
        pszMainInstruction: w!("Download this clipboard copy?"),
        pszContent: details.as_ptr(),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: DECLINE,
        ..Default::default()
    };
    let mut pressed = 0;
    // SAFETY: The CLI's manifest selects Common Controls v6. Load the entry
    // point on demand so other users of the platform crate can run without it.
    let result = unsafe {
        let module = LoadLibraryW(w!("comctl32.dll"));
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        type TaskDialog =
            unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut i32) -> i32;
        let result = GetProcAddress(module, c"TaskDialogIndirect".as_ptr().cast()).map(|entry| {
            let dialog: TaskDialog = std::mem::transmute(entry);
            dialog(&config, &mut pressed, null_mut(), null_mut())
        });
        FreeLibrary(module);
        result.ok_or_else(|| io::Error::other("Common Controls v6 Task Dialog is unavailable"))?
    };
    if result < 0 {
        return Err(io::Error::other(format!(
            "Task Dialog failed (0x{result:08x})"
        )));
    }
    Ok(pressed == DOWNLOAD)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "opens a native file progress dialog on the Windows desktop"]
    fn opens_and_closes_the_native_shell_progress_dialog() {
        let _apartment = Apartment::new().unwrap();
        let mut progress = Progress::new();
        let mut transfer = Transfer {
            id: 1,
            peer: "Test peer".to_owned(),
            label: "sample.bin".to_owned(),
            items: 1,
            phase: Phase::Copying,
            bytes: 250_000,
            total: 1_000_000,
        };
        progress.update(Some(&transfer), 0, &|_| {});
        assert!(
            progress.dialog.is_some(),
            "Windows Shell did not open its progress dialog"
        );
        match &progress.dialog {
            Some(FileDialog::Operations(_)) => {
                println!("Windows Shell file operations dialog opened")
            }
            Some(FileDialog::Standard(_)) => println!("Windows Shell progress dialog opened"),
            None => unreachable!(),
        }
        transfer.phase = Phase::Ready;
        transfer.bytes = transfer.total;
        progress.update(Some(&transfer), 0, &|_| {});
        assert!(progress.dialog.is_none());
    }
}
