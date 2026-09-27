use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::io::AsyncRead;

use super::{
    BoxFuture, DirEntry, EntryKind, Error, InstallResult, MemoryStorage, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile,
};

/// A copy-on-write view of a volume. Reads fall through to the volume, while
/// every mutation stays in memory and is discarded with the view.
#[derive(Debug)]
pub struct ScratchStorage {
    base: Arc<dyn MountableStorage>,
    upper: MemoryStorage,
    removed: Mutex<BTreeSet<String>>,
}

impl ScratchStorage {
    pub fn new(base: Arc<dyn MountableStorage>) -> Self {
        Self {
            base,
            upper: MemoryStorage::writable([]).expect("an empty tree is valid"),
            removed: Mutex::new(BTreeSet::new()),
        }
    }

    fn in_base(&self, path: &str) -> bool {
        !self.removed.lock().contains(path) && self.base.exists(path)
    }

    fn copy_up(&self, path: &str, kind: EntryKind) -> Result<(), Error> {
        if self.upper.exists(path) {
            return Ok(());
        }
        if kind == EntryKind::Directory {
            return self.upper.create_dir_all(path);
        }
        let mut bytes = Vec::new();
        self.base
            .open_read(path)?
            .read_to_end(&mut bytes)
            .map_err(|error| Error::io(Path::new(path), error))?;
        self.upper.create(path)?.write_all_at(0, &bytes)
    }
}

impl Storage for ScratchStorage {
    fn exists(&self, path: &str) -> bool {
        self.upper.exists(path) || self.in_base(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        if self.upper.exists(path) {
            return self.upper.metadata(path);
        }
        if !self.in_base(path) {
            return Err(Error::not_found(path));
        }
        self.base.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if self.metadata(path)?.kind != EntryKind::Directory {
            return Err(Error::Unsupported(format!("`{path}` is not a directory")));
        }
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let mut entries = BTreeMap::<String, DirEntry>::new();
        if self.in_base(path)
            && let Ok(listing) = self.base.read_dir(path, false)
        {
            let removed = self.removed.lock();
            entries.extend(
                listing
                    .filter(|entry| !removed.contains(&entry.path))
                    .map(|entry| (entry.name.clone(), entry)),
            );
        }
        if self.upper.exists(path) {
            entries.extend(
                self.upper
                    .read_dir(path, false)?
                    .map(|entry| (entry.name.clone(), entry)),
            );
        }
        Ok(ReadDir::surface(entries.into_values()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        if self
            .metadata(path)
            .is_ok_and(|metadata| metadata.kind == EntryKind::Directory)
        {
            return Ok(());
        }
        self.upper.create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        self.upper.create(path)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        if !self.upper.exists(path) {
            if !self.in_base(path) {
                return Err(Error::not_found(path));
            }
            if !write {
                return self.base.open(path, false);
            }
            self.copy_up(path, EntryKind::File)?;
        }
        self.upper.open(path, write)
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        let Ok(metadata) = self.metadata(path) else {
            return Ok(());
        };
        if metadata.kind == EntryKind::Directory && self.read_dir(path, false)?.next().is_some() {
            return Err(Error::Unsupported(format!(
                "directory `{path}` is not empty"
            )));
        }
        if self.upper.exists(path) {
            self.upper.remove(path)?;
        }
        if self.base.exists(path) {
            self.removed.lock().insert(path.to_owned());
        }
        Ok(())
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        self.upper.install_file(source, dest)
    }
}

impl MountableStorage for ScratchStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let mut moved = vec![(from.to_owned(), self.metadata(from)?.kind)];
        if moved[0].1 == EntryKind::Directory {
            moved.extend(
                self.read_dir(from, true)?
                    .map(|entry| (entry.path, entry.metadata.kind)),
            );
        }
        for (path, kind) in &moved {
            self.copy_up(path, *kind)?;
        }
        self.upper.rename(from, to)?;
        let mut removed = self.removed.lock();
        removed.extend(
            moved
                .into_iter()
                .map(|(path, _)| path)
                .filter(|path| self.base.exists(path)),
        );
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.open(path, true)?.set_len(len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.metadata(path).map(|_| ())
    }
}
