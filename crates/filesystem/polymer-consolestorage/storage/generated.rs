use super::{
    BoxFuture, DirEntry, EntryKind, Error, InstallResult, Metadata, MountableStorage, ReadDir,
    Storage, StorageFile,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    path::Path,
    sync::Arc,
};
use tokio::io::AsyncRead;

/// Immutable files generated for one mounted session.
pub struct MemoryStorage {
    files: BTreeMap<String, Arc<[u8]>>,
    dirs: BTreeSet<String>,
}

impl MemoryStorage {
    pub fn new(files: impl IntoIterator<Item = (String, Vec<u8>)>) -> Result<Self, Error> {
        let mut result = Self {
            files: BTreeMap::new(),
            dirs: BTreeSet::from([String::new()]),
        };
        for (path, bytes) in files {
            if path.is_empty()
                || path.starts_with('/')
                || path.contains('\\')
                || path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(Error::InvalidPath(path));
            }
            let mut parent = path.as_str();
            while let Some((next, _)) = parent.rsplit_once('/') {
                result.dirs.insert(next.to_owned());
                parent = next;
            }
            if result.files.insert(path.clone(), bytes.into()).is_some() {
                return Err(Error::InvalidPath(path));
            }
        }
        if let Some(path) = result
            .dirs
            .iter()
            .find(|path| result.files.contains_key(*path))
        {
            return Err(Error::InvalidPath(path.clone()));
        }
        Ok(result)
    }

    fn missing(path: &str) -> Error {
        Error::io(Path::new(path), io::Error::from(io::ErrorKind::NotFound))
    }

    fn denied(path: &str) -> Error {
        Error::io(
            Path::new(path),
            io::Error::from(io::ErrorKind::PermissionDenied),
        )
    }
}

impl fmt::Debug for MemoryStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedStorage")
            .field("file_count", &self.files.len())
            .finish()
    }
}

impl Storage for MemoryStorage {
    fn claims(&self, path: &str) -> bool {
        self.files.contains_key(path) || (!path.is_empty() && self.dirs.contains(path))
    }
    fn exists(&self, path: &str) -> bool {
        self.files.contains_key(path) || self.dirs.contains(path)
    }
    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        let (kind, len) = if let Some(bytes) = self.files.get(path) {
            (EntryKind::File, bytes.len() as u64)
        } else if self.dirs.contains(path) {
            (EntryKind::Directory, 0)
        } else {
            return Err(Self::missing(path));
        };
        Ok(Metadata {
            kind,
            len,
            mtime: jiff::Timestamp::UNIX_EPOCH,
        })
    }
    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if !self.dirs.contains(path) {
            return Err(Self::missing(path));
        }
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut children = BTreeSet::new();
        for key in self.dirs.iter().chain(self.files.keys()) {
            if let Some(rest) = key.strip_prefix(&prefix) {
                if let Some(name) = rest.split('/').next().filter(|name| !name.is_empty()) {
                    children.insert(name.to_owned());
                }
            }
        }
        Ok(ReadDir::surface(children.into_iter().map(move |name| {
            let child = format!("{prefix}{name}");
            DirEntry {
                name,
                metadata: self.metadata(&child).expect("generated child exists"),
                path: child,
            }
        })))
    }
    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        if self.dirs.contains(path) {
            Ok(())
        } else {
            Err(Self::denied(path))
        }
    }
    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        Err(Self::denied(path))
    }
    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        if write {
            return Err(Self::denied(path));
        }
        let bytes = self.files.get(path).ok_or_else(|| Self::missing(path))?;
        Ok(Arc::new(GeneratedFile(bytes.clone())))
    }
    fn remove(&self, path: &str) -> Result<(), Error> {
        Err(Self::denied(path))
    }
    fn install_file<'a>(
        &'a self,
        _source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let error = Self::denied(dest);
        Box::pin(async move { Err(error) })
    }
}

impl MountableStorage for MemoryStorage {
    fn rename(&self, from: &str, _to: &str) -> Result<(), Error> {
        Err(Self::denied(from))
    }
    fn truncate(&self, path: &str, _len: u64) -> Result<(), Error> {
        Err(Self::denied(path))
    }
    fn sync(&self, _path: &str) -> Result<(), Error> {
        Ok(())
    }
}

struct GeneratedFile(Arc<[u8]>);

impl StorageFile for GeneratedFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        Ok(Metadata {
            kind: EntryKind::File,
            len: self.0.len() as u64,
            mtime: jiff::Timestamp::UNIX_EPOCH,
        })
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.0.len());
        let available = &self.0[start..];
        let count = available.len().min(buffer.len());
        buffer[..count].copy_from_slice(&available[..count]);
        Ok(count)
    }
    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, Error> {
        Err(MemoryStorage::denied("generated file"))
    }
    fn set_len(&self, _len: u64) -> Result<(), Error> {
        Err(MemoryStorage::denied("generated file"))
    }
    fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
    fn sync(&self) -> Result<(), Error> {
        Ok(())
    }
}
