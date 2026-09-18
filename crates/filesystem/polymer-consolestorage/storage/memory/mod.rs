mod file;

use jiff::Timestamp;
use parking_lot::Mutex;
use std::{collections::BTreeMap, fmt, io, path::Path, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{
    BoxFuture, DirEntry, EntryKind, Error, InstallResult, Metadata, MountableStorage, ReadDir,
    Storage, StorageFile,
};
pub(crate) use file::MemoryFile;

enum Entry {
    Directory(Timestamp),
    File(MemoryFile),
}

impl Entry {
    fn metadata(&self) -> Metadata {
        match self {
            Self::Directory(mtime) => Metadata {
                kind: EntryKind::Directory,
                len: 0,
                mtime: *mtime,
            },
            Self::File(file) => file.metadata(),
        }
    }
}

/// In-memory files and directories, or a single file at the empty path.
pub struct MemoryStorage {
    entries: Mutex<BTreeMap<String, Entry>>,
    writable: bool,
}

impl MemoryStorage {
    /// Builds a read-only tree, suitable for generated NAND files.
    pub fn new(files: impl IntoIterator<Item = (String, Vec<u8>)>) -> Result<Self, Error> {
        let mut storage = Self::writable(files)?;
        storage.writable = false;
        Ok(storage)
    }

    pub fn writable(files: impl IntoIterator<Item = (String, Vec<u8>)>) -> Result<Self, Error> {
        let storage = Self {
            entries: Mutex::new(BTreeMap::from([(
                String::new(),
                Entry::Directory(Timestamp::now()),
            )])),
            writable: true,
        };
        for (path, bytes) in files {
            if path.is_empty() || storage.exists(&path) {
                return Err(Error::InvalidPath(path));
            }
            storage.insert(&path, bytes)?;
        }
        Ok(storage)
    }

    /// Builds a single file, for raw card images or standalone VFS files.
    pub fn file(bytes: Vec<u8>, writable: bool) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::from([(
                String::new(),
                Entry::File(MemoryFile::new(bytes)),
            )])),
            writable,
        }
    }

    /// Copies a file under its byte lock, without exposing the mutable backing.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, Error> {
        Ok(self.entry_file(path)?.bytes())
    }

    fn validate(path: &str) -> Result<(), Error> {
        if !path.is_empty()
            && path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == ".." || part.contains('\\'))
        {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        Ok(())
    }

    fn require_writable(&self, path: &str) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::io(
                Path::new(path),
                io::Error::from(io::ErrorKind::PermissionDenied),
            ));
        }
        Self::validate(path)
    }

    fn entry_file(&self, path: &str) -> Result<MemoryFile, Error> {
        match self.entries.lock().get(path) {
            Some(Entry::File(file)) => Ok(file.clone()),
            Some(Entry::Directory(_)) => Err(Error::Unsupported(format!("`{path}` is not a file"))),
            None => Err(Error::not_found(path)),
        }
    }

    fn ensure_directories(entries: &mut BTreeMap<String, Entry>, path: &str) -> Result<(), Error> {
        let mut parents = vec![path];
        let mut parent = path;
        while let Some((next, _)) = parent.rsplit_once('/') {
            parents.push(next);
            parent = next;
        }
        parents.push("");
        if parents
            .iter()
            .any(|path| matches!(entries.get(*path), Some(Entry::File(_))))
        {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        for path in parents.into_iter().rev() {
            entries
                .entry(path.to_owned())
                .or_insert_with(|| Entry::Directory(Timestamp::now()));
        }
        Ok(())
    }

    fn insert(&self, path: &str, bytes: Vec<u8>) -> Result<(), Error> {
        self.require_writable(path)?;
        let mut entries = self.entries.lock();
        if matches!(entries.get(path), Some(Entry::Directory(_))) {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        if !path.is_empty() {
            Self::ensure_directories(
                &mut entries,
                path.rsplit_once('/').map_or("", |(parent, _)| parent),
            )?;
        }
        entries.insert(path.to_owned(), Entry::File(MemoryFile::new(bytes)));
        Ok(())
    }
}

impl fmt::Debug for MemoryStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryStorage")
            .field("entries", &self.entries.lock().len())
            .field("writable", &self.writable)
            .finish()
    }
}

impl Storage for MemoryStorage {
    fn claims(&self, path: &str) -> bool {
        let entries = self.entries.lock();
        // A generated tree contributes children without hiding the NAND root.
        entries.contains_key(path)
            && (!path.is_empty() || matches!(entries.get(path), Some(Entry::File(_))))
    }

    fn exists(&self, path: &str) -> bool {
        self.entries.lock().contains_key(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.entries
            .lock()
            .get(path)
            .map(Entry::metadata)
            .ok_or_else(|| Error::not_found(path))
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if self.metadata(path)?.kind != EntryKind::Directory {
            return Err(Error::Unsupported(format!("`{path}` is not a directory")));
        }
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let entries = self
            .entries
            .lock()
            .iter()
            .filter_map(|(path, entry)| {
                let name = path.strip_prefix(&prefix)?;
                (!name.is_empty() && !name.contains('/')).then(|| DirEntry {
                    name: name.to_owned(),
                    path: path.clone(),
                    metadata: entry.metadata(),
                })
            })
            .collect::<Vec<_>>();
        Ok(ReadDir::surface(entries.into_iter()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        if self
            .metadata(path)
            .is_ok_and(|meta| meta.kind == EntryKind::Directory)
        {
            return Ok(());
        }
        self.require_writable(path)?;
        Self::ensure_directories(&mut self.entries.lock(), path)
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        self.require_writable(path)?;
        let mut entries = self.entries.lock();
        if !path.is_empty() {
            Self::ensure_directories(
                &mut entries,
                path.rsplit_once('/').map_or("", |(parent, _)| parent),
            )?;
        }
        let entry = entries
            .entry(path.to_owned())
            .or_insert_with(|| Entry::File(MemoryFile::new(Vec::new())));
        let Entry::File(file) = entry else {
            return Err(Error::InvalidPath(path.to_owned()));
        };
        // Truncating an existing file retains the identity of its open handles.
        let handle = file.handle(true);
        handle.set_len(0)?;
        Ok(handle)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        if write {
            self.require_writable(path)?;
        }
        Ok(self.entry_file(path)?.handle(write))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.require_writable(path)?;
        if path.is_empty() {
            return self.open(path, true)?.set_len(0);
        }
        let mut entries = self.entries.lock();
        let prefix = format!("{path}/");
        if entries.keys().any(|entry| entry.starts_with(&prefix)) {
            return Err(Error::Unsupported(format!(
                "directory `{path}` is not empty"
            )));
        }
        entries.remove(path);
        Ok(())
    }

    fn install_file<'a>(
        &'a self,
        mut source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            self.require_writable(&dest)?;
            let mut bytes = Vec::new();
            source
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| Error::io(Path::new(&dest), error))?;
            let result = InstallResult {
                blake3: *blake3::hash(&bytes).as_bytes(),
                byte_length: bytes.len() as u64,
            };
            self.insert(&dest, bytes)?;
            Ok(result)
        })
    }
}

impl MountableStorage for MemoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.require_writable(from)?;
        Self::validate(to)?;
        if from.is_empty()
            || to.is_empty()
            || to.starts_with(&format!("{from}/"))
            || from.starts_with(&format!("{to}/"))
        {
            return Err(Error::InvalidPath(to.to_owned()));
        }
        let mut entries = self.entries.lock();
        if !entries.contains_key(from) {
            return Err(Error::not_found(from));
        }
        if from == to {
            return Ok(());
        }
        Self::ensure_directories(
            &mut entries,
            to.rsplit_once('/').map_or("", |(parent, _)| parent),
        )?;
        let old_prefix = format!("{from}/");
        let paths = entries
            .keys()
            .filter(|path| path.as_str() == from || path.starts_with(&old_prefix))
            .cloned()
            .collect::<Vec<_>>();
        let target_prefix = format!("{to}/");
        entries.retain(|path, _| path != to && !path.starts_with(&target_prefix));
        for path in paths {
            let entry = entries
                .remove(&path)
                .expect("listed entry exists under the tree lock");
            entries.insert(format!("{to}{}", &path[from.len()..]), entry);
        }
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.open(path, true)?.set_len(len)
    }
    fn sync(&self, path: &str) -> Result<(), Error> {
        self.metadata(path).map(|_| ())
    }
}
