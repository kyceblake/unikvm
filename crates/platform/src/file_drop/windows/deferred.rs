//! A Shell virtual file whose contents become available on button release.

use std::{
    io,
    mem::ManuallyDrop,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use windows::{
    Win32::{
        Foundation::{
            DATA_S_SAMEFORMATETC, DV_E_DVASPECT, DV_E_DVTARGETDEVICE, DV_E_FORMATETC, DV_E_LINDEX,
            DV_E_TYMED, E_NOTIMPL, E_POINTER, GlobalFree, OLE_E_ADVISENOTSUPPORTED, S_OK,
            STG_E_ACCESSDENIED,
        },
        System::{
            Com::{
                DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject,
                IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, STGM_READ, STGM_SHARE_DENY_WRITE,
                STGMEDIUM, STGMEDIUM_0, TYMED, TYMED_HGLOBAL, TYMED_ISTREAM,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
        UI::Shell::{
            CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, CFSTR_INDRAGLOOP, FD_FILESIZE, FD_UNICODE,
            FILEDESCRIPTORW, FILEGROUPDESCRIPTORW, SHCreateStdEnumFmtEtc, SHCreateStreamOnFileEx,
        },
    },
    core::{BOOL, Error, HRESULT, HSTRING, PCWSTR, Ref, Result, implement},
};
use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;

#[cfg(feature = "file-drag-probe")]
pub(super) fn file_object(
    path: &std::path::Path,
    released: Arc<AtomicBool>,
) -> io::Result<IDataObject> {
    Ok(VirtualFile::new(path, released)?.into())
}

pub(super) fn received_object(
    name: &str,
    size: u64,
    released: Arc<AtomicBool>,
    verified_path: impl Fn() -> io::Result<PathBuf> + Send + Sync + 'static,
) -> io::Result<IDataObject> {
    Ok(
        VirtualFile::with_source(name.encode_utf16(), size, released, Box::new(verified_path))?
            .into(),
    )
}

#[implement(IDataObject)]
struct VirtualFile {
    path: Box<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
    descriptor: FILEGROUPDESCRIPTORW,
    formats: [FORMATETC; 3],
    released: Arc<AtomicBool>,
}

impl VirtualFile {
    #[cfg(feature = "file-drag-probe")]
    fn new(path: &std::path::Path, released: Arc<AtomicBool>) -> io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        let size = path.metadata()?.len();
        let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let filename = name.encode_wide();
        let path = path.to_owned();
        Self::with_source(
            filename,
            size,
            released,
            Box::new(move || {
                println!("Native target opened the file stream after release.");
                Ok(path.clone())
            }),
        )
    }

    fn with_source(
        name: impl Iterator<Item = u16>,
        size: u64,
        released: Arc<AtomicBool>,
        path: Box<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
    ) -> io::Result<Self> {
        let mut filename = [0; 260];
        for (index, unit) in name.enumerate() {
            if index >= filename.len() - 1 {
                return Err(io::Error::other(
                    "file name exceeds the Shell descriptor limit",
                ));
            }
            filename[index] = unit;
        }
        Ok(Self {
            path,
            descriptor: FILEGROUPDESCRIPTORW {
                cItems: 1,
                fgd: [FILEDESCRIPTORW {
                    dwFlags: (FD_FILESIZE.0 | FD_UNICODE.0) as u32,
                    nFileSizeHigh: (size >> 32) as u32,
                    nFileSizeLow: size as u32,
                    cFileName: filename,
                    ..Default::default()
                }],
            },
            formats: [
                format(CFSTR_FILEDESCRIPTORW, -1, TYMED_HGLOBAL)?,
                format(CFSTR_FILECONTENTS, 0, TYMED_ISTREAM)?,
                format(CFSTR_INDRAGLOOP, -1, TYMED_HGLOBAL)?,
            ],
            released,
        })
    }

    fn check_format(&self, request: &FORMATETC) -> Result<()> {
        let format = self
            .formats
            .iter()
            .find(|f| f.cfFormat == request.cfFormat)
            .ok_or_else(|| Error::from_hresult(DV_E_FORMATETC))?;
        if !request.ptd.is_null() {
            return Err(Error::from_hresult(DV_E_DVTARGETDEVICE));
        }
        if request.dwAspect != format.dwAspect {
            return Err(Error::from_hresult(DV_E_DVASPECT));
        }
        if request.lindex != format.lindex {
            return Err(Error::from_hresult(DV_E_LINDEX));
        }
        if request.tymed & format.tymed == 0 {
            return Err(Error::from_hresult(DV_E_TYMED));
        }
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for VirtualFile_Impl {
    fn GetData(&self, request: *const FORMATETC) -> Result<STGMEDIUM> {
        // SAFETY: COM provides a readable FORMATETC on this apartment's thread.
        let request = unsafe { request.as_ref() }.ok_or_else(|| Error::from_hresult(E_POINTER))?;
        self.check_format(request)?;
        if request.cfFormat == self.formats[0].cfFormat {
            return global_medium(self.descriptor);
        }
        if request.cfFormat == self.formats[2].cfFormat {
            return global_medium(u32::from(!self.released.load(Ordering::Acquire)));
        }
        if !self.released.load(Ordering::Acquire) {
            return Err(Error::from_hresult(STG_E_ACCESSDENIED));
        }
        let path = (self.path)().map_err(|_| Error::from_hresult(STG_E_ACCESSDENIED))?;
        let path = HSTRING::from(path.as_os_str());
        // SAFETY: The path stays valid during the call. The stream opens an existing file for reading.
        let stream = unsafe {
            SHCreateStreamOnFileEx(&path, (STGM_READ | STGM_SHARE_DENY_WRITE).0, 0, false, None)?
        };
        // ReleaseStgMedium owns the stream reference starting at return.
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: ManuallyDrop::new(Some(stream)),
            },
            ..Default::default()
        })
    }

    fn QueryGetData(&self, request: *const FORMATETC) -> HRESULT {
        // SAFETY: COM supplies a readable FORMATETC when non-null.
        match unsafe { request.as_ref() } {
            Some(request) => self
                .check_format(request)
                .map_or_else(|error| error.code(), |()| S_OK),
            None => E_POINTER,
        }
    }

    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(Error::from_hresult(E_NOTIMPL));
        }
        // SAFETY: The Shell copies these fixed FORMATETC values with null target-device pointers.
        unsafe { SHCreateStdEnumFmtEtc(&self.formats) }
    }

    fn GetCanonicalFormatEtc(&self, _input: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        // SAFETY: COM provides writable storage for the output FORMATETC.
        let Some(output) = (unsafe { output.as_mut() }) else {
            return E_POINTER;
        };
        *output = FORMATETC::default();
        DATA_S_SAMEFORMATETC
    }

    fn GetDataHere(&self, _request: *const FORMATETC, _output: *mut STGMEDIUM) -> Result<()> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn SetData(
        &self,
        _format: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: BOOL,
    ) -> Result<()> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn DAdvise(
        &self,
        _format: *const FORMATETC,
        _flags: u32,
        _sink: Ref<'_, IAdviseSink>,
    ) -> Result<u32> {
        Err(Error::from_hresult(OLE_E_ADVISENOTSUPPORTED))
    }

    fn DUnadvise(&self, _connection: u32) -> Result<()> {
        Err(Error::from_hresult(OLE_E_ADVISENOTSUPPORTED))
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(Error::from_hresult(OLE_E_ADVISENOTSUPPORTED))
    }
}

fn format(name: PCWSTR, index: i32, medium: TYMED) -> io::Result<FORMATETC> {
    // SAFETY: The Shell names are static NUL-terminated strings.
    let id = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    if id == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FORMATETC {
        cfFormat: id as u16,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: index,
        tymed: medium.0 as u32,
        ..Default::default()
    })
}

fn global_medium<T: Copy>(value: T) -> Result<STGMEDIUM> {
    // SAFETY: Only plain Shell structures and DWORDs are passed here. GlobalLock provides
    // storage for the full value, and the unlocked handle is transferred to ReleaseStgMedium.
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, size_of::<T>())?;
        let pointer = GlobalLock(handle).cast::<T>();
        if pointer.is_null() {
            let error = Error::from_thread();
            let _ = GlobalFree(Some(handle));
            return Err(error);
        }
        pointer.write_unaligned(value);
        let _ = GlobalUnlock(handle);
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: handle },
            ..Default::default()
        })
    }
}

#[cfg(all(test, feature = "file-drag-probe"))]
mod tests {
    use super::super::{Apartment, FileSource};
    use super::*;
    use windows::Win32::{
        Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, POINTL},
        System::{
            Ole::{DROPEFFECT_COPY, IDropSource, IDropTarget, ReleaseStgMedium},
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
        },
        UI::Shell::{BHID_SFUIObject, IShellItem, SHCreateItemFromParsingName},
    };

    #[test]
    fn virtual_file_exposes_metadata_while_held_and_streams_bytes_on_release() {
        let _ole = Apartment::new().unwrap();
        let sample = tempfile::tempdir().unwrap();
        let name = "sample-画像.txt";
        let path = sample.path().join(name);
        let bytes = b"contents supplied after the drop";
        std::fs::write(&path, bytes).unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let file = VirtualFile::new(&path, released.clone()).unwrap();
        let [descriptor, contents, _] = file.formats;
        let data: IDataObject = file.into();
        let source: IDropSource = FileSource(released).into();

        // SAFETY: COM objects and both returned media belong to this OLE apartment.
        unsafe {
            data.QueryGetData(&contents).ok().unwrap();
            let mut medium = data.GetData(&descriptor).unwrap();
            let handle = medium.u.hGlobal;
            let pointer = GlobalLock(handle).cast::<FILEGROUPDESCRIPTORW>();
            assert!(!pointer.is_null());
            let group = pointer.read_unaligned();
            let _ = GlobalUnlock(handle);
            ReleaseStgMedium(&mut medium);
            let [file] = group.fgd;
            let filename = file.cFileName;
            let units: Vec<_> = name.encode_utf16().chain([0]).collect();
            assert_eq!(&filename[..units.len()], units);

            assert_eq!(source.QueryContinueDrag(false, MK_LBUTTON), S_OK);
            assert_eq!(
                data.GetData(&contents).err().unwrap().code(),
                STG_E_ACCESSDENIED
            );
            assert_eq!(
                source.QueryContinueDrag(true, MK_LBUTTON),
                DRAGDROP_S_CANCEL
            );
            assert_eq!(
                data.GetData(&contents).err().unwrap().code(),
                STG_E_ACCESSDENIED
            );

            // Start a fresh drag with the same metadata and exercise the successful path.
            let next_released = Arc::new(AtomicBool::new(false));
            let next_data = file_object(&path, next_released.clone()).unwrap();
            let next_source: IDropSource = FileSource(next_released).into();
            assert_eq!(
                next_source.QueryContinueDrag(false, MODIFIERKEYS_FLAGS(0)),
                DRAGDROP_S_DROP
            );
            let mut medium = next_data.GetData(&contents).unwrap();
            let mut actual = [0; 32];
            let mut read = 0;
            medium
                .u
                .pstm
                .as_ref()
                .unwrap()
                .Read(
                    actual.as_mut_ptr().cast(),
                    actual.len() as u32,
                    Some(&mut read),
                )
                .ok()
                .unwrap();
            ReleaseStgMedium(&mut medium);
            assert_eq!(&actual[..read as usize], bytes);
        }
    }

    #[test]
    #[ignore = "uses the native Shell drop handler on a Windows desktop"]
    fn shell_folder_copies_the_virtual_file() {
        let _ole = Apartment::new().unwrap();
        let sample = tempfile::tempdir().unwrap();
        let name = "virtual-drop-sample.txt";
        let path = sample.path().join("verified-source.txt");
        let bytes = b"delivered through the Shell folder drop handler";
        std::fs::write(&path, bytes).unwrap();
        let destination = sample.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let data = received_object(name, bytes.len() as u64, released.clone(), {
            let requests = requests.clone();
            let path = path.clone();
            move || {
                requests.fetch_add(1, Ordering::Relaxed);
                Ok(path.clone())
            }
        })
        .unwrap();
        let source: IDropSource = FileSource(released).into();

        // SAFETY: The test owns both locations and the Shell interfaces remain in this apartment.
        unsafe {
            let item: IShellItem =
                SHCreateItemFromParsingName(&HSTRING::from(destination.as_os_str()), None).unwrap();
            let target: IDropTarget = item.BindToHandler(None, &BHID_SFUIObject).unwrap();
            let point = POINTL::default();
            let mut effect = DROPEFFECT_COPY;
            target
                .DragEnter(&data, MK_LBUTTON, point, &mut effect)
                .unwrap();
            assert_eq!(effect, DROPEFFECT_COPY);
            assert_eq!(requests.load(Ordering::Relaxed), 0);
            assert!(!destination.join(name).exists());
            assert_eq!(
                source.QueryContinueDrag(false, MODIFIERKEYS_FLAGS(0)),
                DRAGDROP_S_DROP
            );
            target
                .Drop(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
        }
        assert!(requests.load(Ordering::Relaxed) > 0);
        assert_eq!(std::fs::read(destination.join(name)).unwrap(), bytes);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
