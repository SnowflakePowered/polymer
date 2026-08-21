use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

use crate::{
    BoxFuture, DirEntry, DirectoryStorage, EntryKind, Error, InstallResult, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile, StorageRead,
};

pub(super) const ENTRY: &str = "card.mcd";

pub(super) struct RawMemoryStorage {
    backing: Arc<DirectoryStorage>,
    bytes: Arc<Mutex<Vec<u8>>>,
    reconcile: Arc<dyn Fn(&[u8]) -> Result<(), Error> + Send + Sync>,
}

impl RawMemoryStorage {
    pub(super) fn new(
        backing: Arc<DirectoryStorage>,
        bytes: Vec<u8>,
        reconcile: impl Fn(&[u8]) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Self {
        Self {
            backing,
            bytes: Arc::new(Mutex::new(bytes)),
            reconcile: Arc::new(reconcile),
        }
    }

    fn handle(&self, path: &str) -> Result<MemoryHandle, Error> {
        if path != ENTRY {
            return Err(not_found(path));
        }
        Ok(MemoryHandle {
            bytes: self.bytes.clone(),
            position: 0,
        })
    }
}

impl fmt::Debug for RawMemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ps1RawMemoryStorage")
            .finish_non_exhaustive()
    }
}

impl Storage for RawMemoryStorage {
    fn exists(&self, path: &str) -> bool {
        path.is_empty() || path == ENTRY
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        match path {
            "" => Ok(Metadata {
                kind: EntryKind::Directory,
                len: 0,
                mtime: Timestamp::now(),
            }),
            ENTRY => Ok(Metadata {
                kind: EntryKind::File,
                len: self.bytes.lock().expect("raw card mutex poisoned").len() as u64,
                mtime: Timestamp::now(),
            }),
            _ => Err(not_found(path)),
        }
    }

    fn read_dir(&self, path: &str, _recursive: bool) -> Result<ReadDir<'_>, Error> {
        if !path.is_empty() {
            return Err(not_found(path));
        }
        Ok(ReadDir::surface(
            vec![DirEntry {
                name: ENTRY.to_owned(),
                path: ENTRY.to_owned(),
                kind: EntryKind::File,
            }]
            .into_iter(),
        ))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        if path.is_empty() {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "a raw memory-card view has no directories".to_owned(),
            ))
        }
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.truncate(path, 0)?;
        Ok(Box::new(self.handle(path)?))
    }

    fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        Ok(Box::new(self.handle(path)?))
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        Ok(Box::new(self.handle(path)?))
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        let mut handle = self.handle(path)?;
        handle.position = self.bytes.lock().expect("raw card mutex poisoned").len() as u64;
        Ok(Box::new(handle))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        if path != ENTRY {
            return Err(not_found(path));
        }
        self.bytes.lock().expect("raw card mutex poisoned").clear();
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
        _source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        _dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "files cannot be installed inside a raw memory-card view".to_owned(),
            ))
        })
    }
}

impl MountableStorage for RawMemoryStorage {
    fn rename(&self, _from: &str, _to: &str) -> Result<(), Error> {
        Err(Error::Unsupported(
            "a raw memory-card view has one fixed entry".to_owned(),
        ))
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        if path != ENTRY {
            return Err(not_found(path));
        }
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("raw memory-card image is too large".to_owned()))?;
        self.bytes
            .lock()
            .expect("raw card mutex poisoned")
            .resize(len, 0);
        Ok(())
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        if path != ENTRY {
            return Err(not_found(path));
        }
        let image = self.bytes.lock().expect("raw card mutex poisoned").clone();
        (self.reconcile)(&image)
    }
}

struct MemoryHandle {
    bytes: Arc<Mutex<Vec<u8>>>,
    position: u64,
}

impl Read for MemoryHandle {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let bytes = self.bytes.lock().expect("raw card mutex poisoned");
        let start = usize::try_from(self.position)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let copied = output.len().min(bytes.len() - start);
        output[..copied].copy_from_slice(&bytes[start..start + copied]);
        self.position += copied as u64;
        Ok(copied)
    }
}

impl Write for MemoryHandle {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let mut bytes = self.bytes.lock().expect("raw card mutex poisoned");
        let start = usize::try_from(self.position)
            .map_err(|_| std::io::Error::other("memory-card offset is too large"))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| std::io::Error::other("memory-card write is too large"))?;
        if end > bytes.len() {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(input);
        self.position = end as u64;
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for MemoryHandle {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let len = self.bytes.lock().expect("raw card mutex poisoned").len() as i128;
        let position = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => len + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(u64::MAX)).contains(&position) {
            return Err(std::io::Error::other("invalid memory-card seek"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

fn not_found(path: &str) -> Error {
    Error::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "entry not found"),
    }
}
