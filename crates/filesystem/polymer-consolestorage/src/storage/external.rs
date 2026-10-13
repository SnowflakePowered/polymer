use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncRead;

use super::{
    BoxFuture, DirEntry, DirectoryStorage, EntryKind, InstallResult, Metadata, MountableStorage,
    ReadDir, Storage, StorageFile,
};
use crate::Error;

/// A title directory on the host, mounted at its install path in a NAND.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleMount {
    pub install_path: String,
    pub host: PathBuf,
}

/// Host title directories exposed read-only at their install paths. Each mount
/// claims its whole subtree, so the managed NAND never shows through it, and
/// the directories above it that `base` lacks, so the mount is reachable.
#[derive(Debug)]
pub struct ExternalTitleStorage {
    mounts: Vec<(String, DirectoryStorage)>,
    scaffold: BTreeSet<String>,
}

impl ExternalTitleStorage {
    pub fn new(mounts: Vec<TitleMount>, base: &dyn Storage) -> Self {
        let scaffold = mounts
            .iter()
            .flat_map(|mount| {
                mount
                    .install_path
                    .match_indices('/')
                    .map(|(index, _)| mount.install_path[..index].to_owned())
                    .collect::<Vec<_>>()
            })
            .filter(|ancestor| !base.exists(ancestor))
            .collect();
        Self {
            mounts: mounts
                .into_iter()
                .map(|mount| (mount.install_path, DirectoryStorage::unmanaged(mount.host)))
                .collect(),
            scaffold,
        }
    }

    fn locate<'a>(&self, path: &'a str) -> Option<(&str, &DirectoryStorage, &'a str)> {
        self.mounts.iter().find_map(|(prefix, storage)| {
            let relative = path.strip_prefix(prefix.as_str())?;
            match relative.strip_prefix('/') {
                Some(relative) => Some((prefix.as_str(), storage, relative)),
                None if relative.is_empty() => Some((prefix.as_str(), storage, "")),
                None => None,
            }
        })
    }

    fn denied(path: &str) -> Error {
        Error::io(
            Path::new(path),
            std::io::Error::new(ErrorKind::PermissionDenied, "external titles are read-only"),
        )
    }

    fn missing(path: &str) -> Error {
        Error::io(Path::new(path), ErrorKind::NotFound.into())
    }

    /// Directories above the mounts, so listings of e.g. `usr/title/00050000`
    /// surface mounted titles.
    fn ancestors(&self, path: &str) -> Result<ReadDir<'_>, Error> {
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        for (prefix, storage) in &self.mounts {
            let rest = match path.is_empty() {
                true => Some(prefix.as_str()),
                false => prefix
                    .strip_prefix(path)
                    .and_then(|rest| rest.strip_prefix('/')),
            };
            let Some(name) = rest.and_then(|rest| rest.split('/').next()) else {
                continue;
            };
            if !seen.insert(name.to_owned()) {
                continue;
            }
            let Ok(metadata) = storage.metadata("") else {
                continue;
            };
            entries.push(DirEntry {
                name: name.to_owned(),
                path: match path.is_empty() {
                    true => name.to_owned(),
                    false => format!("{path}/{name}"),
                },
                metadata: Metadata {
                    kind: EntryKind::Directory,
                    ..metadata
                },
            });
        }
        if entries.is_empty() {
            return Err(Self::missing(path));
        }
        Ok(ReadDir::surface(entries.into_iter()))
    }
}

impl Storage for ExternalTitleStorage {
    fn claims(&self, path: &str) -> bool {
        self.scaffold.contains(path) || self.locate(path).is_some()
    }

    fn exists(&self, path: &str) -> bool {
        self.scaffold.contains(path)
            || self
                .locate(path)
                .is_some_and(|(_, storage, relative)| storage.exists(relative))
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        if self.scaffold.contains(path) {
            let metadata = self.mounts[0].1.metadata("")?;
            return Ok(Metadata {
                kind: EntryKind::Directory,
                ..metadata
            });
        }
        let (_, storage, relative) = self.locate(path).ok_or_else(|| Self::missing(path))?;
        storage.metadata(relative)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let Some((prefix, storage, relative)) = self.locate(path) else {
            return self.ancestors(path);
        };
        let prefix = prefix.to_owned();
        Ok(ReadDir::surface(storage.read_dir(relative, false)?.map(
            move |entry| DirEntry {
                path: format!("{prefix}/{}", entry.path),
                ..entry
            },
        )))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        Err(Self::denied(path))
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        Err(Self::denied(path))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        if write {
            return Err(Self::denied(path));
        }
        let (_, storage, relative) = self.locate(path).ok_or_else(|| Self::missing(path))?;
        storage.open(relative, false)
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

impl MountableStorage for ExternalTitleStorage {
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
