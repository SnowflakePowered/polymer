use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, mpsc};

use jiff::Timestamp;
use parking_lot::Mutex;

use crate::impls::RawMemoryCardImage;
use crate::{
    BoxFuture, DirectoryStorage, EntryKind, Error, InstallResult, Metadata, MountableStorage,
    ReadDir, Storage, StorageFile, StorageRead,
};

use super::psu::Save;

pub(super) mod card;

type Reconcile = dyn for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync;

/// A raw PS2 memory-card image exposed as one root file.
///
/// Structured saves are encoded when the view is created and decoded again at
/// each synchronization boundary. The codec is deliberately confined here so
/// it can be replaced without changing the storage or VFS interfaces.
pub(super) struct RawMemoryStorage {
    backing: Arc<DirectoryStorage>,
    image: Arc<CardImage>,
    dirty: mpsc::SyncSender<()>,
}

struct CardImage {
    bytes: Mutex<Vec<u8>>,
    blocks: u32,
    reconcile: Arc<Reconcile>,
    reconciling: Mutex<()>,
}

impl CardImage {
    fn reconcile(&self) -> Result<(), Error> {
        let _reconciling = self.reconciling.lock();
        let image = self.bytes.lock().clone();
        let card = match card::MemoryCard::new(&image, self.blocks) {
            Ok(card) => card,
            Err(_) => return Ok(()),
        };
        let mut saves = match card.saves() {
            Ok(saves) => saves,
            Err(_) => return Ok(()),
        };
        let had_incomplete = saves.iter().any(|save| save.files().len() == 0);
        saves.retain(|save| save.files().len() != 0);
        if had_incomplete && saves.is_empty() {
            return Ok(());
        }
        (self.reconcile)(&saves)
    }
}

impl RawMemoryStorage {
    pub(super) fn new<'a>(
        backing: Arc<DirectoryStorage>,
        capacity_blocks: u32,
        saves: impl IntoIterator<Item = Save<'a>>,
        reconcile: impl for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let saves = saves.into_iter().collect::<Vec<_>>();
        let image = Arc::new(CardImage {
            bytes: Mutex::new(card::MemoryCard::format(capacity_blocks, &saves, true)?),
            blocks: capacity_blocks,
            reconcile: Arc::new(reconcile),
            reconciling: Mutex::new(()),
        });
        let (dirty, pending) = mpsc::sync_channel(1);
        let worker = image.clone();
        std::thread::Builder::new()
            .name("ps2-memory-card".to_owned())
            .spawn(move || {
                while pending.recv().is_ok() {
                    let _ = worker.reconcile();
                }
            })
            .map_err(|source| Error::Io {
                path: "PlayStation 2 memory card".to_owned(),
                source,
            })?;
        Ok(Self {
            backing,
            image,
            dirty,
        })
    }

    fn handle(&self, path: &str) -> Result<MemoryHandle, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        Ok(MemoryHandle {
            image: self.image.clone(),
            dirty: self.dirty.clone(),
            position: 0,
        })
    }
}

impl fmt::Debug for RawMemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ps2RawMemoryStorage")
            .finish_non_exhaustive()
    }
}

impl Storage for RawMemoryStorage {
    fn exists(&self, path: &str) -> bool {
        path.is_empty()
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        Ok(Metadata {
            kind: EntryKind::File,
            len: self.image.bytes.lock().len() as u64,
            mtime: Timestamp::now(),
        })
    }

    fn read_dir(&self, _path: &str, _recursive: bool) -> Result<ReadDir<'_>, Error> {
        Err(Error::Unsupported(
            "a raw memory-card view is a file".to_owned(),
        ))
    }

    fn create_dir_all(&self, _path: &str) -> Result<(), Error> {
        Err(Error::Unsupported(
            "a raw memory-card view is a file".to_owned(),
        ))
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
        handle.position = self.image.bytes.lock().len() as u64;
        Ok(Box::new(handle))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        self.image.bytes.lock().clear();
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
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("raw memory-card image is too large".to_owned()))?;
        self.image.bytes.lock().resize(len, 0);
        Ok(())
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        self.image.reconcile()
    }
}

struct MemoryHandle {
    image: Arc<CardImage>,
    dirty: mpsc::SyncSender<()>,
    position: u64,
}

impl MemoryHandle {
    fn reconcile(&self) {
        let _ = self.dirty.try_send(());
    }
}

impl Read for MemoryHandle {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let bytes = self.image.bytes.lock();
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
        let start = usize::try_from(self.position)
            .map_err(|_| std::io::Error::other("memory-card offset is too large"))?;
        {
            let mut bytes = self.image.bytes.lock();
            let end = start
                .checked_add(input.len())
                .ok_or_else(|| std::io::Error::other("memory-card write is too large"))?;
            if end > bytes.len() {
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(input);
            self.position = end as u64;
        }

        self.reconcile();
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.reconcile();
        Ok(())
    }
}

impl Seek for MemoryHandle {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let len = self.image.bytes.lock().len() as i128;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn save(name: &str, data: &[u8]) -> Save<'static> {
        Save::from_files(name, &[], [("data.bin", &[][..], data)]).unwrap()
    }

    #[tokio::test]
    async fn storage_projects_and_reconciles_structured_saves() {
        let directory = tempfile::tempdir().unwrap();
        let backing = Arc::new(DirectoryStorage::new(directory.path()).await.unwrap());
        let reconciled = Arc::new(Mutex::new(Vec::new()));
        let observed = reconciled.clone();
        let storage = RawMemoryStorage::new(
            backing,
            1024,
            [save("BASLUS-00001", b"first")],
            move |saves| {
                *observed.lock() = saves.iter().map(|save| save.as_bytes().to_vec()).collect();
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(storage.metadata("").unwrap().kind, EntryKind::File);
        assert!(
            storage.image.bytes.lock()[card::RAW_PAGE_SIZE * 8_000..card::RAW_PAGE_SIZE * 8_001]
                .iter()
                .all(|byte| *byte == 0xff)
        );
        let bytes = storage.image.bytes.lock();
        let card = card::MemoryCard::new(&bytes, 1024).unwrap();
        let saves = card.saves().unwrap();
        assert_eq!(saves[0].files().next().unwrap().data(), b"first");
        drop(bytes);

        let replacement_save = save("BASLUS-00002", b"second");
        let replacement = card::MemoryCard::format(1024, &[replacement_save], true).unwrap();
        assert_eq!(
            card::MemoryCard::new(&replacement, 1024)
                .unwrap()
                .saves()
                .unwrap()[0]
                .files()
                .next()
                .unwrap()
                .data(),
            b"second"
        );
        let mut output = storage.open("").unwrap();
        storage.truncate("", 1).unwrap();
        output.write_all(&[0]).unwrap();
        assert!(reconciled.lock().is_empty());

        storage.truncate("", replacement.len() as u64).unwrap();
        output.seek(SeekFrom::Start(0)).unwrap();
        output.write_all(&replacement).unwrap();
        storage.sync("").unwrap();

        let saves = reconciled.lock();
        let save = Save::new(saves[0].as_slice()).unwrap();
        assert_eq!(save.name(), "BASLUS-00002");
        assert_eq!(save.files().next().unwrap().data(), b"second");
    }
}
