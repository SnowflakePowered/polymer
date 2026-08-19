use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek, Write};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::error::Error;

pub mod manifest;

use manifest::{ArtifactDb, ArtifactRegistry};

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
}

/// What [`Storage::install_file`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallResult {
    pub blake3: [u8; 32],
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
}

pub trait StorageFile: Read + Write + Seek + Send {}

impl<T: Read + Write + Seek + Send> StorageFile for T {}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;


pub trait Storage: Send + Sync {
    /// Human-readable name for this storage, used in diagnostics.
    fn describe(&self) -> Cow<'_, str> {
        Cow::Borrowed("storage")
    }

    fn exists(&self, path: &str) -> bool;

    fn metadata(&self, path: &str) -> Result<Metadata, Error>;

    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, Error>;

    fn create_dir_all(&self, path: &str) -> Result<(), Error>;

    /// Create or truncate a file, creating parent directories.
    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error>;

    fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error>;

    /// Open for appending, creating the file and its parents if absent.
    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error>;

    /// Remove a file, or an empty directory. A path that is already gone is not
    /// an error, so uninstalling twice is safe.
    fn remove(&self, path: &str) -> Result<(), Error>;

    /// The record of what is installed here.
    fn artifacts(&self) -> &dyn ArtifactRegistry;

    /// Copy `source` to `dest`, returning the blake3 digest and length of what
    /// was written. The file is only visible at `dest` once fully written.
    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>>;
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
        Ok(Metadata {
            kind: if meta.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            len: meta.len(),
        })
    }

    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let resolved = self.resolve(path)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(&resolved).map_err(|e| Error::io(&resolved, e))? {
            let entry = entry.map_err(|e| Error::io(&resolved, e))?;
            let kind = entry.file_type().map_err(|e| Error::io(&resolved, e))?;
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: if kind.is_dir() {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
            });
        }
        Ok(entries)
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        let resolved = self.resolve(path)?;
        fs::create_dir_all(&resolved).map_err(|e| Error::io(&resolved, e))
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        let resolved = self.resolve(path)?;
        Self::ensure_directory(&resolved)?;
        let file = File::create(&resolved).map_err(|e| Error::io(&resolved, e))?;
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

    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        let resolved = self.resolve(path)?;
        Self::ensure_directory(&resolved)?;
        let file = OpenOptions::new()
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

    fn artifacts(&self) -> &dyn ArtifactRegistry {
        &self.artifacts
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
            copy_hashed(source, &resolved, &dest).await
        })
    }
}

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
