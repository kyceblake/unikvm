use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rustix::process::geteuid;
use tokio::net::{UnixListener, UnixStream};

pub struct Endpoint {
    directory: PathBuf,
}

pub struct Listener {
    socket: UnixListener,
    stream: Option<UnixStream>,
    path: PathBuf,
    _lock: File,
}

impl Endpoint {
    pub fn for_current_user() -> io::Result<Self> {
        let directory =
            if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
                let runtime = PathBuf::from(runtime);
                if !runtime.is_absolute() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "XDG_RUNTIME_DIR must be absolute",
                    ));
                }
                check_directory(&runtime)?;
                runtime.join("unikvm")
            } else {
                // Use a fixed sticky system directory for session tools without XDG_RUNTIME_DIR.
                Path::new("/tmp").join(format!("unikvm-{}", geteuid().as_raw()))
            };
        Ok(Self { directory })
    }

    #[cfg(test)]
    pub(super) fn for_test(namespace: &str) -> io::Result<Self> {
        Ok(Self {
            directory: Path::new("/tmp").join(namespace),
        })
    }

    pub fn listen(self) -> io::Result<Listener> {
        match DirBuilder::new().mode(0o700).create(&self.directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        check_directory(&self.directory)?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(self.directory.join("agent.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "agent already owns the endpoint",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }

        let path = self.socket_path();
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket() && metadata.uid() == geteuid().as_raw() =>
            {
                fs::remove_file(&path)?;
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "unexpected entry at agent socket path",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = UnixListener::bind(&path)?;
        Ok(Listener {
            socket,
            stream: None,
            path,
            _lock: lock,
        })
    }

    pub async fn connect(&self) -> io::Result<UnixStream> {
        check_directory(&self.directory)?;
        let stream = UnixStream::connect(self.socket_path()).await?;
        verify_peer(&stream)?;
        Ok(stream)
    }

    fn socket_path(&self) -> PathBuf {
        self.directory.join("agent.sock")
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn clipboard_directory(&self) -> io::Result<PathBuf> {
        check_directory(&self.directory)?;
        let directory = self.directory.join("clipboard");
        match DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        check_directory(&directory)?;
        Ok(directory)
    }
}

impl Listener {
    pub async fn accept(&mut self) -> io::Result<&mut UnixStream> {
        loop {
            let (stream, _) = self.socket.accept().await?;
            match verify_peer(&stream) {
                Ok(()) => return Ok(self.stream.insert(stream)),
                // macOS can lose peer credentials when a queued client closes
                // before accept. Discard it without exposing an unverified stream.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::NotConnected
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.stream.take();
        Ok(())
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // The lock remains held while the socket path is removed.
        let _ = fs::remove_file(&self.path);
    }
}

fn check_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "IPC directory must be owned by this user with mode 0700",
        ));
    }
    Ok(())
}

fn verify_peer(stream: &UnixStream) -> io::Result<()> {
    if stream.peer_cred()?.uid() != geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "IPC peer belongs to another user",
        ));
    }
    Ok(())
}
