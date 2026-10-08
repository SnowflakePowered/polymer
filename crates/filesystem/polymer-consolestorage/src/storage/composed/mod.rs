mod file;
mod journal;
mod layer;
mod operation;
mod recovery;

pub(crate) use layer::ComposedLayer;

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Weak};

use dashmap::DashMap;
use file::{ComposedFile, ComposedFileBinding};
use journal::RenameJournal;
use operation::ComposedOperation;
use tokio::io::{AsyncRead, AsyncWriteExt};

use crate::installable::StorageWriter;
use crate::{
    BoxFuture, DirEntry, Error, InstallResult, Metadata, MountableStorage, ReadDir, Storage,
    StorageFile, StorageReader,
};

/// A fixed-lifetime, ordered composition. The first storage that claims a path
/// owns it. Directory listings are unioned, but entries are exposed only when
/// they exist in their winning storage.
pub struct ComposedStorage {
    layers: Box<[ComposedLayer]>,
    files: DashMap<String, Weak<ComposedFileBinding>>,
    operation: Arc<ComposedOperation>,
    journal: Option<RenameJournal>,
}

impl ComposedStorage {
    pub(crate) fn with_layers(layers: Vec<ComposedLayer>) -> Result<Self, Error> {
        if layers.is_empty() {
            return Err(Error::Unsupported(
                "a storage composition requires at least one layer".to_owned(),
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        if layers.iter().any(|layer| !ids.insert(&layer.id)) {
            return Err(Error::HistoryConflict(
                "composition layer identities must be unique".to_owned(),
            ));
        }
        let profiles: Vec<_> = layers
            .iter()
            .filter_map(ComposedLayer::recovery_domain)
            .collect();
        if profiles.len() > 1 {
            return Err(Error::Unsupported(
                "a recoverable composition supports one persistent profile".to_owned(),
            ));
        }
        let profile = profiles.into_iter().next();
        let journal = profile
            .as_ref()
            .map(|(domain, profile)| RenameJournal::new(domain.clone(), *profile));
        let storage = Self {
            layers: layers.into_boxed_slice(),
            files: DashMap::new(),
            operation: Arc::new(ComposedOperation::new(profile.map(|(domain, _)| domain))),
            journal,
        };
        storage.recover()?;
        Ok(storage)
    }

    fn binding(&self, path: &str) -> Arc<ComposedFileBinding> {
        let mut entry = self.files.entry(path.to_owned()).or_default();
        if let Some(binding) = entry.upgrade() {
            return binding;
        }
        let binding = Arc::new(ComposedFileBinding::default());
        *entry = Arc::downgrade(&binding);
        binding
    }

    fn file(
        &self,
        path: &str,
        handle: Arc<dyn StorageFile>,
        writable: bool,
    ) -> Arc<dyn StorageFile> {
        Arc::new(ComposedFile {
            handle,
            writable,
            operation: self.operation.clone(),
            binding: self.binding(path),
        })
    }

    fn move_binding(&self, from: &str, to: &str, replacement: Option<Arc<dyn StorageFile>>) {
        let prefix = format!("{from}/");
        let moved: Vec<_> = self
            .files
            .iter()
            .filter_map(|entry| {
                let suffix = entry.key().strip_prefix(&prefix);
                if entry.key() == from || suffix.is_some() {
                    entry.value().upgrade().map(|binding| {
                        let target =
                            suffix.map_or_else(|| to.to_owned(), |suffix| format!("{to}/{suffix}"));
                        (target, binding)
                    })
                } else {
                    None
                }
            })
            .collect();
        self.detach(from);
        self.detach(to);
        for (path, binding) in moved {
            if path == to
                && let Some(handle) = &replacement
            {
                *binding.replacement.write() = Some(handle.clone());
            }
            self.files.insert(path, Arc::downgrade(&binding));
        }
    }

    fn detach(&self, path: &str) {
        let prefix = format!("{path}/");
        self.files.retain(|key, binding| {
            key != path && !key.starts_with(&prefix) && binding.strong_count() > 0
        });
    }

    fn stage(
        &self,
        source: &mut dyn Read,
        owner: &dyn MountableStorage,
        to: &str,
        transaction: Option<uuid::Uuid>,
    ) -> Result<(String, InstallResult, Arc<dyn StorageFile>), Error> {
        self.validate_destination(owner, to)?;
        let (parent, name) = to.rsplit_once('/').unwrap_or(("", to));
        let staged = Self::child(
            parent,
            &format!(
                ".{name}.polymer-{}",
                transaction.unwrap_or_else(uuid::Uuid::now_v7).simple()
            ),
        );
        let result = (|| {
            let target = owner.create(&staged)?;
            let mut writer = StorageWriter {
                file: target.clone(),
                position: 0,
                hash: blake3::Hasher::new(),
            };
            std::io::copy(source, &mut writer)
                .map_err(|error| Error::io(std::path::Path::new(to), error))?;
            target.set_len(writer.position)?;
            target.flush()?;
            target.sync()?;
            Ok((
                InstallResult {
                    blake3: *writer.hash.finalize().as_bytes(),
                    byte_length: writer.position,
                },
                target,
            ))
        })();
        match result {
            Ok((result, target)) => Ok((staged, result, target)),
            Err(error) => {
                let _ = owner.remove(&staged);
                Err(error)
            }
        }
    }

    fn publish(
        &self,
        source: &mut dyn Read,
        owner: &dyn MountableStorage,
        to: &str,
        transaction: Option<uuid::Uuid>,
    ) -> Result<(InstallResult, Arc<dyn StorageFile>), Error> {
        let (staged, result, target) = self.stage(source, owner, to, transaction)?;
        if let Err(error) = owner.rename(&staged, to) {
            let _ = owner.remove(&staged);
            return Err(error);
        }
        Ok((result, target))
    }

    fn transfer(
        &self,
        from: &str,
        to: &str,
        source: &ComposedLayer,
        destination: &ComposedLayer,
    ) -> Result<(), Error> {
        let mut input = StorageReader::new(source.open(from, false)?);
        let (staged, _, target) = self.stage(&mut input, destination.as_ref(), to, None)?;
        if let Err(error) = source.remove(from) {
            let _ = destination.remove(&staged);
            return Err(error);
        }
        if let Err(error) = destination.rename(&staged, to) {
            let restored =
                self.publish(&mut StorageReader::new(target), source.as_ref(), from, None);
            let _ = destination.remove(&staged);
            if let Ok((_, restored)) = restored {
                self.move_binding(from, from, Some(restored));
            }
            return Err(error);
        }
        self.move_binding(from, to, Some(target));
        Ok(())
    }

    fn validate_destination(&self, owner: &dyn MountableStorage, to: &str) -> Result<(), Error> {
        let mut path = to;
        loop {
            let expected = if path == to {
                crate::EntryKind::File
            } else {
                crate::EntryKind::Directory
            };
            match owner.metadata(path) {
                Ok(metadata) if metadata.kind != expected => {
                    return Err(Error::InvalidPath(path.to_owned()));
                }
                Err(error) if !Self::is_not_found(&error) => return Err(error),
                _ => {}
            }
            let Some((parent, _)) = path.rsplit_once('/') else {
                return Ok(());
            };
            path = parent;
        }
    }

    fn owner(&self, path: &str) -> Result<&ComposedLayer, Error> {
        self.layers
            .iter()
            .find(|storage| storage.claims(path))
            .ok_or_else(|| Error::Unsupported(format!("no storage claims `{path}`")))
    }

    fn is_not_found(error: &Error) -> bool {
        matches!(
            error,
            Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
        )
    }

    fn child(parent: &str, name: &str) -> String {
        if parent.is_empty() {
            name.to_owned()
        } else {
            format!("{parent}/{name}")
        }
    }

    fn surface(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let mut candidates = BTreeMap::<String, ()>::new();
        for layer in &self.layers {
            match layer.read_dir(path, false) {
                Ok(entries) => {
                    for entry in entries {
                        candidates.insert(entry.name, ());
                    }
                }
                Err(error) if Self::is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }

        let mut entries = Vec::with_capacity(candidates.len());
        for name in candidates.into_keys() {
            let child = Self::child(path, &name);
            let owner = self.owner(&child)?;
            match owner.metadata(&child) {
                Ok(metadata) => entries.push(DirEntry {
                    name,
                    path: child,
                    metadata,
                }),
                Err(error) if Self::is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(entries)
    }
}

impl fmt::Debug for ComposedStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComposedStorage")
            .field("layers", &self.layers)
            .finish()
    }
}

impl Storage for ComposedStorage {
    fn exists(&self, path: &str) -> bool {
        let Ok(_operation) = self.operation.read() else {
            return false;
        };
        self.owner(path).is_ok_and(|storage| storage.exists(path))
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        let _operation = self.operation.read()?;
        self.owner(path)?.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let _operation = self.operation.read()?;
        Ok(ReadDir::surface(self.surface(path)?.into_iter()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        let _operation = self.operation.write()?;
        self.owner(path)?.create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        let _operation = self.operation.write()?;
        let handle = self.owner(path)?.create(path)?;
        Ok(self.file(path, handle, true))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        let _operation = self.operation.read()?;
        let handle = self.owner(path)?.open(path, write)?;
        Ok(self.file(path, handle, write))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        let _operation = self.operation.write()?;
        self.owner(path)?.remove(path)?;
        self.detach(path);
        Ok(())
    }

    fn install_file<'a>(
        &'a self,
        mut source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            let temporary = tempfile::tempfile()
                .map_err(|error| Error::io(std::path::Path::new(&dest), error))?;
            let mut temporary = tokio::fs::File::from_std(temporary);
            tokio::io::copy(&mut source, &mut temporary)
                .await
                .map_err(|error| Error::io(std::path::Path::new(&dest), error))?;
            temporary
                .flush()
                .await
                .map_err(|error| Error::io(std::path::Path::new(&dest), error))?;
            let mut temporary = temporary.into_std().await;
            temporary
                .seek(SeekFrom::Start(0))
                .map_err(|error| Error::io(std::path::Path::new(&dest), error))?;
            let _operation = self.operation.write()?;
            let (result, _) =
                self.publish(&mut temporary, self.owner(&dest)?.as_ref(), &dest, None)?;
            self.detach(&dest);
            Ok(result)
        })
    }
}

impl MountableStorage for ComposedStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let _operation = self.operation.write()?;
        let from_owner = self.owner(from)?;
        let to_owner = self.owner(to)?;
        let profile = if from_owner.persistent() {
            from_owner
        } else {
            to_owner
        };
        let _profile = profile.acquire()?;
        if Arc::ptr_eq(from_owner, to_owner) {
            from_owner.rename(from, to)?;
            self.move_binding(from, to, None);
            return profile.persist_changes();
        }

        let metadata = from_owner.metadata(from)?;
        if metadata.kind != crate::EntryKind::File {
            return Err(Error::Unsupported(
                "renaming a directory across composed storage layers is unsupported".to_owned(),
            ));
        }

        if profile.persistent() {
            self.journaled_rename(from, to, from_owner, to_owner)
        } else {
            self.transfer(from, to, from_owner, to_owner)
        }
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        let _operation = self.operation.write()?;
        self.owner(path)?.truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.recover()?;
        let _operation = self.operation.write()?;
        let layer = self.owner(path)?;
        let _domain = layer.acquire()?;
        layer.sync(path)?;
        layer.persist_changes()
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        self.recover()?;
        let _operation = self.operation.write()?;
        for layer in &self.layers {
            layer.finish(path)?;
        }
        Ok(())
    }
}
