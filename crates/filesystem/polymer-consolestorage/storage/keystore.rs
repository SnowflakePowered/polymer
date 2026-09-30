use std::{collections::BTreeMap, fmt, sync::Arc};

use jiff::Timestamp;
use parking_lot::Mutex;
use tokio::io::AsyncRead;

use super::{
    BoxFuture, DirEntry, EntryKind, Error, InstallResult, MemoryStorage, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile,
};
use crate::keys::{KeyDescription, KeyQuery, KeyReader, KeyRecord};

/// Maps a platform's content keys to the files its NAND expects.
pub trait KeyProjection: Send + Sync {
    /// The NAND path for `key`, or `None` when this platform cannot use it.
    fn path(&self, key: &KeyDescription) -> Option<String>;

    /// The length of the generated file, known without reading the key.
    fn len(&self, key: &KeyDescription) -> u64;

    fn render(&self, key: &KeyRecord) -> Result<Vec<u8>, Error>;
}

/// Content keys projected into a NAND. Only key metadata is listed when the
/// storage is created; a key is read from the store the first time its file is
/// opened and then kept, with any guest writes, for the rest of the session.
pub struct KeyStoreStorage {
    keys: Arc<dyn KeyReader>,
    projection: Box<dyn KeyProjection>,
    pending: Mutex<BTreeMap<String, (KeyDescription, Timestamp)>>,
    files: MemoryStorage,
}

impl KeyStoreStorage {
    pub async fn new(
        keys: Arc<dyn KeyReader>,
        projection: impl KeyProjection + 'static,
    ) -> Result<Self, Error> {
        let files = MemoryStorage::writable([])?;
        let mut pending = BTreeMap::new();
        let listed = keys
            .list(&KeyQuery {
                namespace: Some("content".into()),
                title_id: None,
            })
            .await
            .map_err(|error| Error::Unsupported(error.to_string()))?;
        for key in listed {
            let Some(path) = projection.path(&key) else {
                continue;
            };
            if let Some((parent, _)) = path.rsplit_once('/') {
                files.create_dir_all(parent)?;
            }
            pending.insert(path, (key, Timestamp::now()));
        }
        Ok(Self {
            keys,
            projection: Box::new(projection),
            pending: Mutex::new(pending),
            files,
        })
    }

    fn load(&self, path: &str) -> Result<(), Error> {
        let mut pending = self.pending.lock();
        let Some((key, _)) = pending.get(path) else {
            return Ok(());
        };
        let record = futures::executor::block_on(self.keys.read(&key.identity))
            .map_err(|error| Error::Unsupported(error.to_string()))?;
        let bytes = self.projection.render(&record)?;
        self.files.create(path)?.write_all_at(0, &bytes)?;
        pending.remove(path);
        Ok(())
    }
}

impl fmt::Debug for KeyStoreStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyStoreStorage")
            .field("pending", &self.pending.lock().len())
            .field("files", &self.files)
            .finish()
    }
}

impl Storage for KeyStoreStorage {
    fn claims(&self, path: &str) -> bool {
        self.pending.lock().contains_key(path) || self.files.claims(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.pending.lock().contains_key(path) || self.files.exists(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        match self.pending.lock().get(path) {
            Some((key, mtime)) => Ok(Metadata {
                kind: EntryKind::File,
                len: self.projection.len(key),
                mtime: *mtime,
            }),
            None => self.files.metadata(path),
        }
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let mut entries: Vec<DirEntry> = self.files.read_dir(path, false)?.collect();
        let parent = |child: &str| child.rsplit_once('/').map_or("", |(parent, _)| parent) == path;
        entries.extend(
            self.pending
                .lock()
                .iter()
                .filter(|(child, _)| parent(child))
                .map(|(child, (key, mtime))| DirEntry {
                    name: child.rsplit('/').next().unwrap_or(child).to_owned(),
                    path: child.clone(),
                    metadata: Metadata {
                        kind: EntryKind::File,
                        len: self.projection.len(key),
                        mtime: *mtime,
                    },
                }),
        );
        Ok(ReadDir::surface(entries.into_iter()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        self.files.create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        self.pending.lock().remove(path);
        self.files.create(path)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        self.load(path)?;
        self.files.open(path, write)
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.pending.lock().remove(path);
        self.files.remove(path)
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        self.pending.lock().remove(dest);
        self.files.install_file(source, dest)
    }
}

impl MountableStorage for KeyStoreStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.load(from)?;
        self.pending.lock().remove(to);
        self.files.rename(from, to)
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.load(path)?;
        self.files.truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        if self.pending.lock().contains_key(path) {
            return Ok(());
        }
        self.files.sync(path)
    }
}
