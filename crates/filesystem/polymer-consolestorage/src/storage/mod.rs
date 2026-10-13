use std::fmt;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek};

use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use std::sync::Arc;

use uuid::Uuid;

use crate::error::Error;
use atomic_write_file::AtomicWriteFile;

mod composed;
mod external;
mod git;
mod keystore;
pub mod manifest;
pub(crate) mod memory;
pub mod memorycard;
pub(crate) mod operation;
pub(crate) mod permissions;
mod reconcile;
mod scratch;

pub(crate) use composed::ComposedLayer;
pub use composed::ComposedStorage;
pub use external::{ExternalTitleStorage, TitleMount};
pub use git::{GitVolumeStorage, PlatformRepository};
pub(crate) use git::{PendingScope, WritePass};
pub use keystore::{KeyProjection, KeyStoreStorage};
pub use memory::{MemoryFile, MemoryStorage};
pub use reconcile::{ReconcileStorage, ReconciliableStorage};
pub use scratch::ScratchStorage;

use crate::storage::manifest::VolumeInfo;
use manifest::ArtifactRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Declared with a trailing `/`.
    Directory,
    /// Declared without a trailing `/`.
    File,
}

impl EntryKind {
    pub(crate) fn matches(self, meta: &fs::Metadata) -> bool {
        match self {
            EntryKind::Directory => meta.is_dir(),
            EntryKind::File => meta.is_file(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub kind: EntryKind,
    pub len: u64,
    pub mtime: jiff::Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallResult {
    pub blake3: [u8; 32],
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub metadata: Metadata,
}

/// A directory listing.
pub enum ReadDir<'a> {
    Surface {
        entries: Box<dyn Iterator<Item = DirEntry> + Send + 'a>,
    },
    Recursive {
        storage: &'a dyn Storage,
        current: Box<dyn Iterator<Item = DirEntry> + Send + 'a>,
        pending: Vec<String>,
    },
}

impl<'a> ReadDir<'a> {
    pub fn surface(entries: impl Iterator<Item = DirEntry> + Send + 'a) -> Self {
        Self::Surface {
            entries: Box::new(entries),
        }
    }

    pub fn recursive(storage: &'a dyn Storage, root: &str) -> Result<Self, Error> {
        Ok(Self::Recursive {
            storage,
            current: storage.read_dir(root, false)?.entries(),
            pending: Vec::new(),
        })
    }

    fn entries(self) -> Box<dyn Iterator<Item = DirEntry> + Send + 'a> {
        match self {
            Self::Surface { entries } => entries,
            recursive => Box::new(recursive),
        }
    }
}

impl Iterator for ReadDir<'_> {
    type Item = DirEntry;

    fn next(&mut self) -> Option<DirEntry> {
        match self {
            Self::Surface { entries } => entries.next(),
            Self::Recursive {
                storage,
                current,
                pending,
            } => loop {
                if let Some(entry) = current.next() {
                    if entry.metadata.kind == EntryKind::Directory {
                        pending.push(entry.path.clone());
                    }

                    return Some(entry);
                }

                // An unreadable subdirectory is skipped rather than ending the walk.
                match storage.read_dir(&pending.pop()?, false) {
                    Ok(listing) => *current = listing.entries(),
                    Err(_) => continue,
                }
            },
        }
    }
}

pub trait ChangeSignal: Send + Sync {
    fn changed(&self) -> BoxFuture<'_, ()>;
}

/// A position-independent file handle suitable for concurrent filesystem
/// callbacks. Implementations must not maintain a shared cursor.
pub trait StorageFile: Send + Sync {
    fn metadata(&self) -> Result<Metadata, Error>;

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error>;

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, Error>;

    fn write_all_at(&self, mut offset: u64, mut buffer: &[u8]) -> Result<(), Error> {
        while !buffer.is_empty() {
            let written = self.write_at(offset, buffer)?;
            if written == 0 {
                return Err(Error::Unsupported(
                    "storage file write made no progress".to_owned(),
                ));
            }
            offset = offset
                .checked_add(written as u64)
                .ok_or_else(|| Error::Unsupported("storage file offset is too large".to_owned()))?;
            buffer = &buffer[written..];
        }
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), Error>;

    fn flush(&self) -> Result<(), Error>;

    fn sync(&self) -> Result<(), Error>;
}

/// A sequential cursor over a position-independent storage handle. This exists
/// for parsers that consume [`Read`] and [`Seek`]; mounted I/O continues to use
/// [`StorageFile`] directly.
pub struct StorageReader {
    file: Arc<dyn StorageFile>,
    position: u64,
}

impl StorageReader {
    pub fn new(file: Arc<dyn StorageFile>) -> Self {
        Self { file, position: 0 }
    }
}

impl Read for StorageReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let read = self
            .file
            .read_at(self.position, output)
            .map_err(std::io::Error::other)?;
        self.position = self
            .position
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("storage file position is too large"))?;
        Ok(read)
    }
}

impl Seek for StorageReader {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        let len = i128::from(self.file.metadata().map_err(std::io::Error::other)?.len);
        let position = match position {
            std::io::SeekFrom::Start(offset) => i128::from(offset),
            std::io::SeekFrom::End(offset) => len + i128::from(offset),
            std::io::SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(u64::MAX)).contains(&position) {
            return Err(std::io::Error::other("invalid storage file seek"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Storage: fmt::Debug + Send + Sync {
    /// Whether this storage owns `path` when it participates in an ordered
    /// composition. Claiming is independent of whether the path exists: an
    /// empty higher-priority storage must still hide lower-priority contents.
    fn claims(&self, path: &str) -> bool {
        let _ = path;
        true
    }

    fn exists(&self, path: &str) -> bool;

    fn metadata(&self, path: &str) -> Result<Metadata, Error>;

    /// List `path`, optionally walking everything beneath it.
    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error>;

    fn create_dir_all(&self, path: &str) -> Result<(), Error>;

    /// Create or truncate a file, creating parent directories.
    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error>;

    /// Open a cursor-free handle. Reads are always available; `write` controls
    /// whether mutation methods are permitted.
    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error>;

    /// Open a sequential cursor for parsers that consume [`Read`] and [`Seek`].
    fn open_read(&self, path: &str) -> Result<StorageReader, Error> {
        Ok(StorageReader::new(self.open(path, false)?))
    }

    /// Remove a file, or an empty directory. A path that is already gone is not
    /// an error, so uninstalling twice is safe.
    fn remove(&self, path: &str) -> Result<(), Error>;

    /// Returns a signal for mutations made through this storage.
    fn watch(&self) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        Ok(None)
    }

    fn install_bytes(&self, _bytes: &[u8], _dest: &str) -> Result<InstallResult, Error> {
        Err(Error::Unsupported(
            "synchronous atomic installation is not supported".to_owned(),
        ))
    }

    /// Copy `source` to `dest`, returning the blake3 digest and length of what
    /// was written. The file is only visible at `dest` once fully written.
    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>>;
}

/// The operations a guest filesystem needs that plain enumeration does not.
/// A volume only has to satisfy these to be mounted.
pub trait MountableStorage: Storage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error>;

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error>;

    fn sync(&self, path: &str) -> Result<(), Error>;

    /// Flushes final state and stops backend work owned by this mounted view.
    fn finish(&self, path: &str) -> Result<(), Error> {
        self.sync(path)
    }
}

/// A mountable filesystem with persistent volume identity and artifact metadata.
pub trait VolumeStorage: MountableStorage {
    /// Whether the checkout differs from the committed volume contents.
    fn is_dirty(&self) -> Result<bool, Error> {
        Err(Error::Unsupported("checkout inspection".into()))
    }

    /// Explicitly discard all uncommitted changes and restore the committed NAND.
    fn recover(&self) -> Result<(), Error> {
        Err(Error::Unsupported("NAND recovery".into()))
    }

    /// User-facing name for this volume, if one has been assigned.
    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>>;

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>>;

    /// The record of what is installed here. Save-only volumes have none.
    fn artifacts(&self) -> &dyn ArtifactRegistry;

    /// Who this volume is, or `None` if nothing has branded it yet.
    fn volumeinfo(&self) -> BoxFuture<'_, Result<Option<VolumeInfo>, Error>>;

    /// Establish an identity if there is none, returning it either way.
    fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<VolumeInfo, Error>>;
}

/// A volume whose filesystem and volume operations are provided by another volume.
pub(crate) trait VolumeOverlay: fmt::Debug + Send + Sync {
    fn inner(&self) -> &dyn VolumeStorage;

    fn claims(&self, path: &str) -> bool {
        self.inner().claims(path)
    }
}

impl<T: VolumeOverlay> Storage for T {
    fn claims(&self, path: &str) -> bool {
        VolumeOverlay::claims(self, path)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner().exists(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.inner().metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        self.inner().read_dir(path, recursive)
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        self.inner().create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        self.inner().create(path)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        self.inner().open(path, write)
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.inner().remove(path)
    }

    fn watch(&self) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        self.inner().watch()
    }

    fn install_bytes(&self, bytes: &[u8], dest: &str) -> Result<InstallResult, Error> {
        self.inner().install_bytes(bytes, dest)
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        self.inner().install_file(source, dest)
    }
}

impl<T: VolumeOverlay> MountableStorage for T {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.inner().rename(from, to)
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.inner().truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.inner().sync(path)
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        self.inner().finish(path)
    }
}

impl<T: VolumeOverlay> VolumeStorage for T {
    fn is_dirty(&self) -> Result<bool, Error> {
        self.inner().is_dirty()
    }

    fn recover(&self) -> Result<(), Error> {
        self.inner().recover()
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        self.inner().label()
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        self.inner().set_label(label)
    }

    fn artifacts(&self) -> &dyn ArtifactRegistry {
        self.inner().artifacts()
    }

    fn volumeinfo(&self) -> BoxFuture<'_, Result<Option<VolumeInfo>, Error>> {
        self.inner().volumeinfo()
    }

    fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<VolumeInfo, Error>> {
        self.inner().brand(id, platform, label)
    }
}

pub struct DirectoryStorage {
    root: PathBuf,
}

impl DirectoryStorage {
    pub(crate) fn unmanaged(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub async fn new(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;

        Ok(Self::unmanaged(root))
    }

    /// Opens a directory strictly as a data store. Volume identity and other
    /// metadata are supplied by an outer storage implementation.
    pub async fn new_unmanaged(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
        Ok(Self::unmanaged(root))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, Error> {
        if path.is_empty() {
            return Ok(self.root.clone());
        }
        for component in path.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(Error::InvalidPath(path.to_string()));
            }
            if component.contains('\\') {
                return Err(Error::InvalidPath(path.to_string()));
            }
            if component == ".git" {
                return Err(Error::InvalidPath(path.to_string()));
            }
        }
        Ok(self.root.join(path))
    }

    fn ensure_directory(resolved: &Path) -> Result<(), Error> {
        match resolved.parent() {
            Some(parent) => fs::create_dir_all(parent).map_err(|e| Error::io(parent, e)),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for DirectoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectoryStorage")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Storage for DirectoryStorage {
    fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok_and(|p| p.exists())
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        let resolved = self.resolve(path)?;
        let meta = fs::metadata(&resolved).map_err(|e| Error::io(&resolved, e))?;
        let mtime = meta.modified().map_err(|e| Error::io(&resolved, e))?;
        Ok(Metadata {
            kind: if meta.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            len: meta.len(),
            mtime: jiff::Timestamp::try_from(mtime)?,
        })
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if recursive {
            return ReadDir::recursive(self, path);
        }

        let resolved = self.resolve(path)?;
        let listing = fs::read_dir(&resolved).map_err(|e| Error::io(&resolved, e))?;
        let parent = path.to_string();

        Ok(ReadDir::surface(listing.filter_map(move |entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                return None;
            }
            let path = match parent.is_empty() {
                true => name.clone(),
                false => format!("{parent}/{name}"),
            };

            let meta = entry.metadata().ok()?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|value| jiff::Timestamp::try_from(value).ok())?;
            Some(DirEntry {
                name,
                path,
                metadata: Metadata {
                    kind: if meta.is_dir() {
                        EntryKind::Directory
                    } else if meta.is_file() {
                        EntryKind::File
                    } else {
                        return None;
                    },
                    len: meta.len(),
                    mtime,
                },
            })
        })))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        let resolved = self.resolve(path)?;
        fs::create_dir_all(&resolved).map_err(|e| Error::io(&resolved, e))
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        let resolved = self.resolve(path)?;
        Self::ensure_directory(&resolved)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&resolved)
            .map_err(|e| Error::io(&resolved, e))?;
        Ok(Arc::new(NativeStorageFile {
            file,
            path: resolved,
            writable: true,
        }))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        let resolved = self.resolve(path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .open(&resolved)
            .map_err(|error| Error::io(&resolved, error))?;
        Ok(Arc::new(NativeStorageFile {
            file,
            path: resolved,
            writable: write,
        }))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        if path.is_empty() {
            return Err(Error::InvalidPath(path.to_string()));
        }

        let resolved = self.resolve(path)?;
        let removed = match fs::symlink_metadata(&resolved) {
            Ok(meta) if meta.is_dir() => fs::remove_dir(&resolved),
            Ok(_) => fs::remove_file(&resolved),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::io(&resolved, e)),
        };

        match removed {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&resolved, e)),
        }
    }

    fn install_bytes(&self, bytes: &[u8], dest: &str) -> Result<InstallResult, Error> {
        use std::io::Write;

        if dest.is_empty() {
            return Err(Error::InvalidPath(dest.to_owned()));
        }
        let resolved = self.resolve(dest)?;
        Self::ensure_directory(&resolved)?;
        let mut output =
            AtomicWriteFile::open(&resolved).map_err(|error| Error::io(&resolved, error))?;
        output
            .write_all(bytes)
            .map_err(|error| Error::io(&resolved, error))?;
        output
            .commit()
            .map_err(|error| Error::io(&resolved, error))?;
        Ok(InstallResult {
            blake3: *blake3::hash(bytes).as_bytes(),
            byte_length: bytes.len() as u64,
        })
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let resolved = if dest.is_empty() {
            Err(Error::InvalidPath(dest.to_string()))
        } else {
            self.resolve(dest)
        };

        let dest = dest.to_string();
        Box::pin(async move {
            let resolved = resolved?;
            DirectoryStorage::copy_hashed(source, &resolved, &dest).await
        })
    }
}

struct NativeStorageFile {
    file: fs::File,
    path: PathBuf,
    writable: bool,
}

impl StorageFile for NativeStorageFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        let metadata = self
            .file
            .metadata()
            .map_err(|error| Error::io(&self.path, error))?;
        let modified = metadata
            .modified()
            .map_err(|error| Error::io(&self.path, error))?;
        Ok(Metadata {
            kind: EntryKind::File,
            len: metadata.len(),
            mtime: jiff::Timestamp::try_from(modified)?,
        })
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        native_read_at(&self.file, offset, buffer).map_err(|error| Error::io(&self.path, error))
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, Error> {
        if !self.writable {
            return Err(Error::io(
                &self.path,
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only handle"),
            ));
        }
        native_write_at(&self.file, offset, buffer).map_err(|error| Error::io(&self.path, error))
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::io(
                &self.path,
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only handle"),
            ));
        }
        self.file
            .set_len(len)
            .map_err(|error| Error::io(&self.path, error))
    }

    fn flush(&self) -> Result<(), Error> {
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.file
            .sync_all()
            .map_err(|error| Error::io(&self.path, error))
    }
}

#[cfg(windows)]
fn native_read_at(file: &fs::File, offset: u64, buffer: &mut [u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buffer, offset)
}

#[cfg(windows)]
fn native_write_at(file: &fs::File, offset: u64, buffer: &[u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buffer, offset)
}

#[cfg(unix)]
fn native_read_at(file: &fs::File, offset: u64, buffer: &mut [u8]) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buffer, offset)
}

#[cfg(unix)]
fn native_write_at(file: &fs::File, offset: u64, buffer: &[u8]) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buffer, offset)
}

impl DirectoryStorage {
    /// Copy `source` onto `resolved` through an atomic writer, hashing in the same pass.
    async fn copy_hashed(
        mut source: Box<dyn AsyncRead + Send + Unpin>,
        resolved: &Path,
        dest: &str,
    ) -> Result<InstallResult, Error> {
        let parent = resolved
            .parent()
            .ok_or_else(|| Error::InvalidPath(dest.to_string()))?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::io(parent, e))?;

        let writer = AtomicWriteFile::open(resolved).map_err(|e| Error::io(resolved, e))?;
        let file = writer
            .as_file()
            .try_clone()
            .map_err(|e| Error::io(resolved, e))?;
        let mut output = tokio::fs::File::from_std(file);

        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut byte_length = 0u64;

        loop {
            let read = match source.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => read,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::io(resolved, e)),
            };

            hasher.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .await
                .map_err(|e| Error::io(resolved, e))?;
            byte_length += read as u64;
        }
        output.flush().await.map_err(|e| Error::io(resolved, e))?;
        drop(output);
        writer.commit().map_err(|e| Error::io(resolved, e))?;

        Ok(InstallResult {
            blake3: *hasher.finalize().as_bytes(),
            byte_length,
        })
    }
}

impl MountableStorage for DirectoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let from = self.resolve(from)?;
        let to = self.resolve(to)?;
        Self::ensure_directory(&to)?;
        fs::rename(&from, &to).map_err(|e| Error::io(&from, e))
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        let resolved = self.resolve(path)?;
        OpenOptions::new()
            .write(true)
            .open(&resolved)
            .and_then(|file| file.set_len(len))
            .map_err(|e| Error::io(&resolved, e))
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        let resolved = self.resolve(path)?;
        OpenOptions::new()
            .write(true)
            .open(&resolved)
            .and_then(|file| file.sync_all())
            .map_err(|e| Error::io(&resolved, e))
    }
}
