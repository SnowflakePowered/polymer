use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;
use tokio::io::AsyncReadExt;

use super::gci::{self, Save};
use crate::impls::memorycard::ReconcileWorker;
use crate::{
    BoxFuture, DirEntry, DirectoryStorage, EntryKind, Error, InstallResult, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile, StorageRead, StorageStream,
};

type Reconcile = dyn for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync;

#[derive(Clone)]
struct Entry(Arc<Mutex<EntryState>>);

struct EntryState {
    bytes: Vec<u8>,
    mtime: Timestamp,
}

impl Entry {
    fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(EntryState {
            bytes,
            mtime: Timestamp::now(),
        })))
    }

    fn metadata(&self) -> Metadata {
        let state = self.0.lock();
        Metadata {
            kind: EntryKind::File,
            len: state.bytes.len() as u64,
            mtime: state.mtime,
        }
    }
}

struct Projection {
    entries: Mutex<BTreeMap<String, Entry>>,
    reconcile: Box<Reconcile>,
}

impl Projection {
    fn saves(&self) -> Result<Option<Vec<Save<'static>>>, Error> {
        let entries = self.entries.lock();
        let mut identities = HashSet::new();
        let mut saves = Vec::new();
        for (path, entry) in entries.iter() {
            if !path.to_ascii_lowercase().ends_with(".gci") {
                continue;
            }
            let state = entry.0.lock();
            if state.bytes.len() < gci::DIRENT_SIZE {
                return Ok(None);
            }
            let blocks = u16::from_be_bytes([state.bytes[0x38], state.bytes[0x39]]);
            let expected = gci::DIRENT_SIZE + usize::from(blocks) * gci::BLOCK_SIZE;
            if state.bytes.len() < expected {
                return Ok(None);
            }
            let save =
                Save::new(state.bytes.clone()).map_err(|error| Error::MalformedMetadata {
                    file: path.clone(),
                    reason: error.to_string(),
                })?;
            if !identities.insert(save.identity()) {
                return Err(Error::Unsupported(
                    "a Dolphin memory-card folder contains duplicate save identities".to_owned(),
                ));
            }
            saves.push(save);
        }
        Ok(Some(saves))
    }

    fn commit(&self) -> Result<(), Error> {
        if let Some(saves) = self.saves()? {
            (self.reconcile)(&saves)?;
        }
        Ok(())
    }
}

pub(super) struct DolphinMemoryStorage {
    backing: Arc<DirectoryStorage>,
    projection: Arc<Projection>,
    worker: ReconcileWorker,
}

impl DolphinMemoryStorage {
    pub(super) fn new(
        backing: Arc<DirectoryStorage>,
        saves: &[Save<'_>],
        reconcile: impl for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let mut entries = BTreeMap::new();
        for save in saves {
            let mut name = save.file_name();
            while entries.contains_key(&name) {
                name.insert(name.len() - 4, '0');
            }
            entries.insert(name, Entry::new(save.as_bytes().to_vec()));
        }
        let projection = Arc::new(Projection {
            entries: Mutex::new(entries),
            reconcile: Box::new(reconcile),
        });
        let background = Arc::downgrade(&projection);
        let worker = ReconcileWorker::spawn(
            "snowglass-gci-writeback",
            "Dolphin memory card",
            move || {
                background
                    .upgrade()
                    .map_or(Ok(()), |projection| projection.commit())
            },
        )?;
        Ok(Self {
            backing,
            projection,
            worker,
        })
    }

    fn entry(&self, path: &str) -> Result<Entry, Error> {
        self.projection
            .entries
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| Error::not_found(path))
    }

    fn handle(&self, path: &str, position: u64) -> Result<Handle, Error> {
        let entry = self.entry(path)?;
        Ok(Handle {
            entry,
            position,
            worker: self.worker.clone(),
        })
    }
}

impl fmt::Debug for DolphinMemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DolphinMemoryStorage")
            .field("backing", &self.backing)
            .finish_non_exhaustive()
    }
}

impl Storage for DolphinMemoryStorage {
    fn exists(&self, path: &str) -> bool {
        path.is_empty() || self.projection.entries.lock().contains_key(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        if path.is_empty() {
            return Ok(Metadata {
                kind: EntryKind::Directory,
                len: 0,
                mtime: Timestamp::now(),
            });
        }
        Ok(self.entry(path)?.metadata())
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let entries = self
            .projection
            .entries
            .lock()
            .iter()
            .map(|(name, entry)| DirEntry {
                name: name.clone(),
                path: name.clone(),
                metadata: entry.metadata(),
            })
            .collect::<Vec<_>>();
        Ok(ReadDir::surface(entries.into_iter()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        if path.is_empty() {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "Dolphin memory-card folders cannot contain directories".to_owned(),
            ))
        }
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
        if path.is_empty() || path.contains('/') {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        let entry = Entry::new(Vec::new());
        self.projection
            .entries
            .lock()
            .insert(path.to_owned(), entry.clone());
        Ok(Box::new(Handle {
            entry,
            position: 0,
            worker: self.worker.clone(),
        }))
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        Ok(Box::new(self.handle(path, 0)?))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        Ok(Arc::new(DolphinStorageFile {
            entry: self.entry(path)?,
            worker: self.worker.clone(),
            writable: write,
        }))
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
        if !self.exists(path) {
            return self.create(path);
        }
        Ok(Box::new(self.handle(path, self.metadata(path)?.len)?))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.projection.entries.lock().remove(path);
        self.worker.schedule();
        Ok(())
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        self.backing.label()
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        self.backing.set_label(label)
    }

    fn install_file<'a>(
        &'a self,
        mut source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            let mut bytes = Vec::new();
            source
                .read_to_end(&mut bytes)
                .await
                .map_err(|source| Error::Io {
                    path: dest.clone(),
                    source,
                })?;
            let result = InstallResult {
                blake3: *blake3::hash(&bytes).as_bytes(),
                byte_length: bytes.len() as u64,
            };
            self.create(&dest)?
                .write_all(&bytes)
                .map_err(|source| Error::Io { path: dest, source })?;
            Ok(result)
        })
    }
}

impl MountableStorage for DolphinMemoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let mut entries = self.projection.entries.lock();
        let entry = entries.remove(from).ok_or_else(|| Error::not_found(from))?;
        if !to.to_ascii_lowercase().ends_with(".deleted") {
            entries.insert(to.to_owned(), entry);
        }
        drop(entries);
        self.worker.schedule();
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        let entry = self.entry(path)?;
        let mut state = entry.0.lock();
        state.bytes.resize(
            usize::try_from(len)
                .map_err(|_| Error::Unsupported("GCI file is too large".to_owned()))?,
            0,
        );
        state.mtime = Timestamp::now();
        drop(state);
        self.worker.schedule();
        Ok(())
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            self.entry(path)?;
        }
        self.worker.reconcile()
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            self.entry(path)?;
        }
        self.worker.shutdown()
    }
}

struct Handle {
    entry: Entry,
    position: u64,
    worker: ReconcileWorker,
}

impl Read for Handle {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let state = self.entry.0.lock();
        let start = usize::try_from(self.position)
            .unwrap_or(usize::MAX)
            .min(state.bytes.len());
        let copied = output.len().min(state.bytes.len() - start);
        output[..copied].copy_from_slice(&state.bytes[start..start + copied]);
        self.position += copied as u64;
        Ok(copied)
    }
}

impl Write for Handle {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let start = usize::try_from(self.position)
            .map_err(|_| std::io::Error::other("GCI offset is too large"))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| std::io::Error::other("GCI write is too large"))?;
        {
            let mut state = self.entry.0.lock();
            if end > state.bytes.len() {
                state.bytes.resize(end, 0);
            }
            state.bytes[start..end].copy_from_slice(input);
            state.mtime = Timestamp::now();
        }
        self.position = end as u64;
        self.worker.schedule();
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.worker.reconcile().map_err(std::io::Error::other)
    }
}

struct DolphinStorageFile {
    entry: Entry,
    worker: ReconcileWorker,
    writable: bool,
}

impl StorageFile for DolphinStorageFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        Ok(self.entry.metadata())
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
        let state = self.entry.0.lock();
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(state.bytes.len());
        let copied = output.len().min(state.bytes.len() - start);
        output[..copied].copy_from_slice(&state.bytes[start..start + copied]);
        Ok(copied)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, Error> {
        if !self.writable {
            return Err(Error::Unsupported(
                "Dolphin file handle is read-only".to_owned(),
            ));
        }
        let start = usize::try_from(offset)
            .map_err(|_| Error::Unsupported("GCI offset is too large".to_owned()))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| Error::Unsupported("GCI write is too large".to_owned()))?;
        let mut state = self.entry.0.lock();
        if end > state.bytes.len() {
            state.bytes.resize(end, 0);
        }
        state.bytes[start..end].copy_from_slice(input);
        state.mtime = Timestamp::now();
        drop(state);
        self.worker.schedule();
        Ok(input.len())
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::Unsupported(
                "Dolphin file handle is read-only".to_owned(),
            ));
        }
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("GCI file is too large".to_owned()))?;
        let mut state = self.entry.0.lock();
        state.bytes.resize(len, 0);
        state.mtime = Timestamp::now();
        drop(state);
        self.worker.schedule();
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        self.worker.schedule();
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.worker.reconcile()
    }
}

impl Seek for Handle {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let len = self.entry.0.lock().bytes.len() as i128;
        let position = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => len + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(u64::MAX)).contains(&position) {
            return Err(std::io::Error::other("invalid GCI seek"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}
