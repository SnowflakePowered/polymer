use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek, Write};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;

use std::sync::Arc;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Notify;

use uuid::Uuid;

use crate::error::Error;

pub mod manifest;
pub(crate) mod permissions;

use manifest::{ArtifactDb, ArtifactRegistry};
use crate::storage::manifest::NandMetadata;

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
    pub kind: EntryKind,
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
                    if entry.kind == EntryKind::Directory {
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

pub trait StorageFile: Read + Write + Seek + Send {}

impl<T: Read + Write + Seek + Send> StorageFile for T {}

pub trait StorageRead: Read + Seek + Send {}

impl<T: Read + Seek + Send> StorageRead for T {}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;


pub trait Storage: Send + Sync {
    /// Human-readable name for this storage, used in diagnostics.
    fn describe(&self) -> Cow<'_, str> {
        Cow::Borrowed("storage")
    }

    fn exists(&self, path: &str) -> bool;

    fn metadata(&self, path: &str) -> Result<Metadata, Error>;

    /// List `path`, optionally walking everything beneath it.
    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error>;

    fn create_dir_all(&self, path: &str) -> Result<(), Error>;

    /// Create or truncate a file, creating parent directories.
    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error>;

    /// Open for reading and writing.
    fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error>;

    /// Open for reading only, which succeeds on entries [`Self::open`] rejects.
    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error>;

    /// Open for appending, creating the file and its parents if absent.
    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error>;

    /// Remove a file, or an empty directory. A path that is already gone is not
    /// an error, so uninstalling twice is safe.
    fn remove(&self, path: &str) -> Result<(), Error>;

    /// Returns a recursive change watcher for the requested paths.
    fn watch(&self, paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        let _ = paths;
        Ok(None)
    }

    /// The record of what is installed here. Save-only volumes have none.
    fn artifacts(&self) -> &dyn ArtifactRegistry;

    /// Who this volume is, or `None` if nothing has branded it yet.
    fn volumeinfo(&self) -> BoxFuture<'_, Result<Option<NandMetadata>, Error>>;

    /// Establish an identity if there is none, returning it either way.
    fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<NandMetadata, Error>>;

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>>;

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>>;

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

    fn set_len(&self, path: &str, len: u64) -> Result<(), Error>;

    fn sync(&self, path: &str) -> Result<(), Error>;
}

pub struct DirectoryStorage {
    root: PathBuf,
    artifacts: ArtifactDb,
}

impl DirectoryStorage {
    pub async fn new(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;

        let database = root.join(".manifest.db");
        let artifacts = ArtifactDb::open(&database.to_string_lossy()).await?;

        Ok(Self { root, artifacts })
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

impl Storage for DirectoryStorage {
    fn describe(&self) -> Cow<'_, str> {
        self.root.to_string_lossy()
    }

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
            mtime: jiff::Timestamp::try_from(mtime)?
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
            let path = match parent.is_empty() {
                true => name.clone(),
                false => format!("{parent}/{name}"),
            };

            Some(DirEntry {
                name,
                path,
                kind: match entry.file_type().ok()?.is_dir() {
                    true => EntryKind::Directory,
                    false => EntryKind::File,
                },
            })
        })))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        let resolved = self.resolve(path)?;
        fs::create_dir_all(&resolved).map_err(|e| Error::io(&resolved, e))
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        let resolved = self.resolve(path)?;
        Self::ensure_directory(&resolved)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&resolved)
            .map_err(|e| Error::io(&resolved, e))?;
        Ok(Box::new(file))
    }

    fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        let resolved = self.resolve(path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&resolved)
            .map_err(|e| Error::io(&resolved, e))?;
        Ok(Box::new(file))
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        let resolved = self.resolve(path)?;
        let file = OpenOptions::new()
            .read(true)
            .open(&resolved)
            .map_err(|e| Error::io(&resolved, e))?;
        Ok(Box::new(file))
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        let resolved = self.resolve(path)?;
        Self::ensure_directory(&resolved)?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&resolved)
            .map_err(|e| Error::io(&resolved, e))?;
        Ok(Box::new(file))
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

    fn watch(&self, paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        let signal = Arc::new(Notify::new());
        let notifier = Arc::clone(&signal);

        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            // Ignore aaccess events
            if event.is_ok_and(|event| event.kind.is_access()) {
                return;
            }

            notifier.notify_one();
        })
        .map_err(|e|  Error::io(&self.root, std::io::Error::other(e)))?;

        for path in paths {
            let resolved = self.resolve(path)?;
            watcher
                .watch(&resolved, RecursiveMode::Recursive)
                .map_err(|e| Error::io(&self.root, std::io::Error::other(e)))?;
        }

        Ok(Some(Box::new(DirectoryWatch {
            _watcher: watcher,
            signal,
        })))
    }

    fn artifacts(&self) -> &dyn ArtifactRegistry {
        &self.artifacts
    }

    fn volumeinfo(&self) -> BoxFuture<'_, Result<Option<NandMetadata>, Error>> {
        self.artifacts.identity()
    }

    fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<NandMetadata, Error>> {
        self.artifacts.brand(id, platform, label)
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        Box::pin(async move {
            Ok(self
                .artifacts
                .identity()
                .await?
                .and_then(|metadata| metadata.label))
        })
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        self.artifacts.set_label(label)
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

impl DirectoryStorage {
    /// Copy `source` onto `resolved` through a staging file, hashing in the same pass.
    async fn copy_hashed(
        mut source: Box<dyn AsyncRead + Send + Unpin>,
        resolved: &Path,
        dest: &str,
    ) -> Result<InstallResult, Error> {
        // for atomic copying
        fn staging_path(resolved: &Path) -> PathBuf {
            let mut name = resolved.file_name().unwrap_or_default().to_os_string();
            name.push(".sgpart");
            resolved.with_file_name(name)
        }

        let parent = resolved
            .parent()
            .ok_or_else(|| Error::InvalidPath(dest.to_string()))?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::io(parent, e))?;

        let staged = staging_path(resolved);
        let mut writer = tokio::fs::File::create(&staged)
            .await
            .map_err(|e| Error::io(&staged, e))?;

        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut byte_length = 0u64;

        let copied = async {
            loop {
                let read = match source.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                };

                hasher.update(&buffer[..read]);
                writer.write_all(&buffer[..read]).await?;
                byte_length += read as u64;
            }

            writer.flush().await
        }
            .await;
        drop(writer);

        if let Err(error) = copied {
            let _ = tokio::fs::remove_file(&staged).await;
            return Err(Error::io(&staged, error));
        }

        if let Err(error) = tokio::fs::rename(&staged, resolved).await {
            let _ = tokio::fs::remove_file(&staged).await;
            return Err(Error::io(resolved, error));
        }

        Ok(InstallResult {
            blake3: *hasher.finalize().as_bytes(),
            byte_length,
        })
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_joins_relative_paths_and_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(dir.path()).await.unwrap();

        assert_eq!(storage.resolve("").unwrap(), dir.path());
        assert_eq!(
            storage.resolve("usr/title/00050000101c9400/code/app.xml").unwrap(),
            dir.path().join("usr/title/00050000101c9400/code/app.xml")
        );

        for path in ["/abs", "..", "../escape", "a/../b", "a/./b", "a//b", "a/", r"a\b"] {
            assert!(
                matches!(storage.resolve(path), Err(Error::InvalidPath(_))),
                "`{path}` was not rejected"
            );
        }
    }
}

/// Keeps the OS watch alive; dropping it stops the watch.
struct DirectoryWatch {
    _watcher: RecommendedWatcher,
    signal: Arc<Notify>,
}

impl ChangeSignal for DirectoryWatch {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.signal.notified())
    }
}

impl MountableStorage for DirectoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let from = self.resolve(from)?;
        let to = self.resolve(to)?;
        Self::ensure_directory(&to)?;
        fs::rename(&from, &to).map_err(|e| Error::io(&from, e))
    }

    fn set_len(&self, path: &str, len: u64) -> Result<(), Error> {
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
