use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Weak};

use parking_lot::{Mutex, RwLock};
use tokio::io::AsyncRead;

use crate::{
    BoxFuture, DirEntry, Error, InstallResult, Metadata, MountableStorage, ReadDir, Storage,
    StorageFile,
};

/// A fixed-lifetime, ordered composition. The first storage that claims a path
/// owns it. Directory listings are unioned, but entries are exposed only when
/// they exist in their winning storage.
pub struct ComposedStorage {
    layers: Box<[Arc<dyn MountableStorage>]>,
    files: Mutex<HashMap<String, Weak<ComposedFileBinding>>>,
}

#[derive(Default)]
struct ComposedFileBinding {
    /// Set when a native handle could not follow a rename across layers.
    redirected_to: RwLock<Option<String>>,
}

impl ComposedStorage {
    pub fn new(layers: Vec<Arc<dyn MountableStorage>>) -> Result<Self, Error> {
        if layers.is_empty() {
            return Err(Error::Unsupported(
                "a storage composition requires at least one layer".to_owned(),
            ));
        }
        Ok(Self {
            layers: layers.into_boxed_slice(),
            files: Mutex::new(HashMap::new()),
        })
    }

    fn binding(&self, path: &str) -> Arc<ComposedFileBinding> {
        let mut files = self.files.lock();
        if let Some(binding) = files.get(path).and_then(Weak::upgrade) {
            return binding;
        }
        let binding = Arc::new(ComposedFileBinding::default());
        files.insert(path.to_owned(), Arc::downgrade(&binding));
        binding
    }

    fn file(&self, path: &str, handle: Arc<dyn StorageFile>) -> Arc<dyn StorageFile> {
        Arc::new(ComposedFile {
            layers: self.layers.clone(),
            opened_at: path.to_owned(),
            handle,
            binding: self.binding(path),
        })
    }

    fn move_binding(&self, from: &str, to: &str, crossed_layers: bool) {
        let mut files = self.files.lock();
        let source = files.remove(from).and_then(|binding| binding.upgrade());
        files.remove(to);
        if let Some(binding) = source {
            if crossed_layers || binding.redirected_to.read().is_some() {
                *binding.redirected_to.write() = Some(to.to_owned());
            }
            files.insert(to.to_owned(), Arc::downgrade(&binding));
        }
    }

    fn owner(&self, path: &str) -> Result<&Arc<dyn MountableStorage>, Error> {
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
        self.owner(path).is_ok_and(|storage| storage.exists(path))
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.owner(path)?.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if recursive {
            return ReadDir::recursive(self, path);
        }
        Ok(ReadDir::surface(self.surface(path)?.into_iter()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        self.owner(path)?.create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        let handle = self.owner(path)?.create(path)?;
        Ok(self.file(path, handle))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        let handle = self.owner(path)?.open(path, write)?;
        Ok(self.file(path, handle))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.owner(path)?.remove(path)
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        match self.owner(dest) {
            Ok(storage) => storage.install_file(source, dest),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

impl MountableStorage for ComposedStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let from_owner = self.owner(from)?;
        let to_owner = self.owner(to)?;
        if Arc::ptr_eq(from_owner, to_owner) {
            from_owner.rename(from, to)?;
            self.move_binding(from, to, false);
            return Ok(());
        }

        let metadata = from_owner.metadata(from)?;
        if metadata.kind != crate::EntryKind::File {
            return Err(Error::Unsupported(
                "renaming a directory across composed storage layers is unsupported".to_owned(),
            ));
        }

        // Stage the complete file beside its destination, then use the
        // destination layer's native rename for the visible replacement. A
        // guest never observes a partially copied file even when a rename
        // crosses the composition boundary (for example Wii /tmp -> save data).
        let (parent, name) = to.rsplit_once('/').unwrap_or(("", to));
        let staged_name = format!(".{name}.polymer-{}", uuid::Uuid::now_v7().simple());
        let staged = Self::child(parent, &staged_name);
        let copy = (|| {
            let source = from_owner.open(from, false)?;
            let target = to_owner.create(&staged)?;
            let mut offset = 0_u64;
            let mut buffer = vec![0_u8; 64 * 1024];
            while offset < metadata.len {
                let remaining = usize::try_from((metadata.len - offset).min(buffer.len() as u64))
                    .expect("a bounded buffer length fits usize");
                let read = source.read_at(offset, &mut buffer[..remaining])?;
                if read == 0 {
                    return Err(Error::Unsupported(format!(
                        "storage file `{from}` ended before its declared length"
                    )));
                }
                target.write_all_at(offset, &buffer[..read])?;
                offset += read as u64;
            }
            target.set_len(metadata.len)?;
            target.flush()?;
            target.sync()?;
            to_owner.rename(&staged, to)
        })();
        if let Err(error) = copy {
            let _ = to_owner.remove(&staged);
            return Err(error);
        }

        // The destination is already complete and visible. Failure to clean
        // the old temporary file must not turn a successful atomic publish
        // into a guest-visible failed rename.
        let _ = from_owner.remove(from);
        self.move_binding(from, to, true);
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.owner(path)?.truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.owner(path)?.sync(path)
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        for layer in &self.layers {
            layer.finish(path)?;
        }
        Ok(())
    }
}

struct ComposedFile {
    layers: Box<[Arc<dyn MountableStorage>]>,
    opened_at: String,
    handle: Arc<dyn StorageFile>,
    binding: Arc<ComposedFileBinding>,
}

impl ComposedFile {
    fn redirected(&self) -> Option<(Arc<dyn MountableStorage>, String)> {
        let path = self.binding.redirected_to.read().clone()?;
        if path == self.opened_at {
            return None;
        }
        let owner = self
            .layers
            .iter()
            .find(|storage| storage.claims(&path))?
            .clone();
        Some((owner, path))
    }
}

impl StorageFile for ComposedFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        match self.redirected() {
            Some((owner, path)) => owner.metadata(&path),
            None => self.handle.metadata(),
        }
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        match self.redirected() {
            Some((owner, path)) => owner.open(&path, false)?.read_at(offset, buffer),
            None => self.handle.read_at(offset, buffer),
        }
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, Error> {
        match self.redirected() {
            Some((owner, path)) => owner.open(&path, true)?.write_at(offset, buffer),
            None => self.handle.write_at(offset, buffer),
        }
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        match self.redirected() {
            Some((owner, path)) => owner.truncate(&path, len),
            None => self.handle.set_len(len),
        }
    }

    fn flush(&self) -> Result<(), Error> {
        match self.redirected() {
            Some((owner, path)) => owner.open(&path, true)?.flush(),
            None => self.handle.flush(),
        }
    }

    fn sync(&self) -> Result<(), Error> {
        match self.redirected() {
            Some((owner, path)) => owner.sync(&path),
            None => self.handle.sync(),
        }
    }
}
