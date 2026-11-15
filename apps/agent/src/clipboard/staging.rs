use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::time::Instant;
use unikvm_platform::clipboard::Clipboard;
use unikvm_transfer as transfer;

const GRACE: Duration = Duration::from_secs(600);

#[derive(Default)]
pub(super) struct Staging {
    pub(super) directory: Option<PathBuf>,
    pub(super) sequence: u64,
    current: Option<PathBuf>,
    previous: Option<(PathBuf, Instant)>,
    sweep: Option<Instant>,
}

impl Staging {
    pub(super) fn publish(&mut self, path: Option<PathBuf>, sequence: u64) {
        if let Some(old) = self.current.take() {
            if let Some((previous, _)) = self.previous.take() {
                let _ = fs::remove_dir_all(previous);
            }
            self.previous = Some((old, Instant::now() + GRACE));
        }
        self.current = path;
        self.sequence = sequence;
    }

    pub(super) fn reconcile(&mut self, os: &Clipboard) {
        if !os.available() {
            return;
        }
        if let Some(directory) = self.directory.as_ref()
            && self.sweep.is_none()
            && let Ok(sequence) = os.sequence()
            && let Ok(paths) = os.imported_files()
            && os.sequence().ok() == Some(sequence)
        {
            self.sequence = sequence;
            self.current = paths.and_then(|paths| {
                let path = paths.first()?.strip_prefix(directory).ok()?;
                let name = path.iter().next()?.to_str()?;
                name.starts_with(transfer::STAGING_PREFIX)
                    .then(|| directory.join(name))
            });
            self.sweep = Some(Instant::now() + GRACE);
        }
        self.expire();
        if self.current.is_some()
            && let Ok(Some(snapshot)) = os.read(self.sequence)
        {
            if snapshot.imported {
                self.sequence = snapshot.sequence;
            } else {
                self.publish(None, snapshot.sequence);
            }
        }
    }

    pub(super) fn expire(&mut self) {
        let now = Instant::now();
        if self
            .previous
            .as_ref()
            .is_some_and(|(_, deadline)| *deadline <= now)
            && let Some((path, _)) = self.previous.take()
        {
            let _ = fs::remove_dir_all(path);
        }
        if self.sweep.is_some_and(|deadline| deadline <= now) {
            self.sweep = Some(now + Duration::from_secs(60));
            if let Some(directory) = self.directory.as_ref() {
                prune(
                    directory,
                    self.current.as_deref(),
                    self.previous.as_ref().map(|(path, _)| path.as_path()),
                );
            }
        }
    }
}

fn prune(directory: &Path, current: Option<&Path>, previous: Option<&Path>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if Some(path.as_path()) == current
            || Some(path.as_path()) == previous
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(transfer::STAGING_PREFIX))
            || !entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        if entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age >= GRACE))
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}
