//! On-disk identity storage for the manual pairing milestone.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use tempfile::NamedTempFile;
use zeroize::Zeroizing;

#[cfg(target_os = "windows")]
mod windows;

pub fn create(path: &Path, secret: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)?;
    #[cfg(target_os = "windows")]
    let sealed = windows::protect(secret)?;
    #[cfg(target_os = "windows")]
    let secret = &sealed;

    temporary.write_all(secret)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

pub fn read(path: &Path, limit: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    let file = {
        use std::fs::OpenOptions;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity file must be user-owned with mode 0600 or stricter",
            ));
        }
        file
    };
    #[cfg(target_os = "windows")]
    let file = File::open(path)?;

    #[cfg(unix)]
    let bytes = read_bounded(file, limit)?;
    #[cfg(target_os = "windows")]
    let bytes = windows::unprotect(&read_bounded(file, limit.saturating_add(4096))?)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity file exceeds size limit",
        ));
    }
    Ok(bytes)
}

fn read_bounded(file: File, limit: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity file exceeds size limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::{create, read};

    #[test]
    fn credentials_round_trip_and_creation_does_not_replace_existing_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        let payload = b"credential storage test";
        create(&path, payload).unwrap();
        assert_eq!(&**read(&path, 1024).unwrap(), payload);
        assert_eq!(read(&path, 4).unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(
            create(&path, b"new key").unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(&**read(&path, 1024).unwrap(), payload);
        #[cfg(target_os = "windows")]
        assert!(
            !std::fs::read(path)
                .unwrap()
                .windows(payload.len())
                .any(|chunk| chunk == payload)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                read(&path, 1024).unwrap_err().kind(),
                ErrorKind::PermissionDenied
            );
        }
    }
}
