use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;
use tokio::io::AsyncReadExt;

use crate::impls::memorycard::ReconcileWorker;
use crate::{
    BoxFuture, ChangeSignal, DirEntry, DirectoryStorage, EntryKind, Error, InstallResult, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile, StorageRead, StorageStream,
};

use super::psu::Save;
use super::raw;

const SUPERBLOCK: &str = "_pcsx2_superblock";
const DELETED_PREFIX: &str = "_pcsx2_deleted_";

type Reconcile = dyn Fn(&[Save<'static>]) -> Result<(), Error> + Send + Sync;

#[derive(Clone)]
enum Entry {
    Directory {
        mtime: Arc<Mutex<Timestamp>>,
    },
    File {
        bytes: Arc<Mutex<Vec<u8>>>,
        mtime: Arc<Mutex<Timestamp>>,
    },
}

impl Entry {
    fn directory() -> Self {
        Self::Directory {
            mtime: Arc::new(Mutex::new(Timestamp::now())),
        }
    }

    fn file(bytes: Vec<u8>) -> Self {
        Self::File {
            bytes: Arc::new(Mutex::new(bytes)),
            mtime: Arc::new(Mutex::new(Timestamp::now())),
        }
    }

    fn metadata(&self) -> Metadata {
        match self {
            Self::Directory { mtime } => Metadata {
                kind: EntryKind::Directory,
                len: 0,
                mtime: *mtime.lock(),
            },
            Self::File { bytes, mtime } => Metadata {
                kind: EntryKind::File,
                len: bytes.lock().len() as u64,
                mtime: *mtime.lock(),
            },
        }
    }
}

/// An in-memory PCSX2 folder-card projection of PSU-backed saves.
pub(super) struct Pcsx2MemoryStorage {
    backing: Arc<DirectoryStorage>,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    worker: ReconcileWorker,
}

impl Pcsx2MemoryStorage {
    pub(super) fn new<'a>(
        backing: Arc<DirectoryStorage>,
        capacity_blocks: u32,
        saves: impl IntoIterator<Item = Save<'a>>,
        reconcile: impl Fn(&[Save<'static>]) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let superblock = raw::card::MemoryCard::formatted_superblock(capacity_blocks)?;
        let mut entries = BTreeMap::from([(SUPERBLOCK.to_owned(), Entry::file(superblock))]);
        for save in saves {
            Self::project_save(&mut entries, &save);
        }
        let entries = Arc::new(Mutex::new(entries));
        let reconcile = Arc::new(reconcile) as Arc<Reconcile>;
        let background_entries = entries.clone();
        let background_reconcile = reconcile.clone();
        let worker = ReconcileWorker::spawn(
            "snowglass-pcsx2-writeback",
            "PCSX2 memory card",
            move || background_reconcile(&Self::saves(&background_entries)?),
        )?;
        Ok(Self {
            backing,
            entries,
            worker,
        })
    }

    fn project_save(entries: &mut BTreeMap<String, Entry>, save: &Save<'_>) {
        let save_name = save.name().into_owned();
        entries.insert(save_name.clone(), Entry::directory());
        entries.insert(
            format!("{save_name}/_pcsx2_meta_directory"),
            Entry::file(save.raw_dirent().to_vec()),
        );
        let metadata = format!("{save_name}/_pcsx2_meta");
        entries.insert(metadata.clone(), Entry::directory());
        let mut index = String::from("{\"$ROOT\":{\"timeCreated\":0,\"timeModified\":0}");
        for (order, file) in save.files().enumerate() {
            let file_name = file.name().into_owned();
            entries.insert(
                format!("{save_name}/{file_name}"),
                Entry::file(file.data().to_vec()),
            );
            entries.insert(
                format!("{metadata}/{file_name}"),
                Entry::file(file.raw_dirent().to_vec()),
            );
            index.push(',');
            index.push_str(&serde_json::to_string(&file_name).expect("a string serializes"));
            index.push_str(":{\"order\":");
            index.push_str(&order.to_string());
            index.push_str(",\"timeCreated\":0,\"timeModified\":0}");
        }
        index.push('}');
        entries.insert(
            format!("{save_name}/_pcsx2_index"),
            Entry::file(index.into_bytes()),
        );
    }

    fn is_private(path: &str) -> bool {
        path.split('/').any(|part| part.starts_with("_pcsx2_"))
    }

    fn create_parents(entries: &mut BTreeMap<String, Entry>, path: &str) {
        let mut prefix = String::new();
        let count = path.split('/').count();
        for component in path.split('/').take(count.saturating_sub(1)) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            entries
                .entry(prefix.clone())
                .or_insert_with(Entry::directory);
        }
    }

    fn entry(&self, path: &str) -> Result<Entry, Error> {
        self.entries
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| Error::not_found(path))
    }

    fn children(&self, path: &str) -> Vec<DirEntry> {
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        self.entries
            .lock()
            .iter()
            .filter_map(|(entry_path, entry)| {
                let suffix = entry_path.strip_prefix(&prefix)?;
                (!suffix.is_empty() && !suffix.contains('/')).then(|| DirEntry {
                    name: suffix.to_owned(),
                    path: entry_path.clone(),
                    metadata: entry.metadata(),
                })
            })
            .collect()
    }

    fn bytes(entries: &BTreeMap<String, Entry>, path: &str) -> Vec<u8> {
        match entries.get(path) {
            Some(Entry::File { bytes, .. }) => bytes.lock().clone(),
            _ => Vec::new(),
        }
    }

    fn dirent(entries: &BTreeMap<String, Entry>, path: &str, name: &str) -> Vec<u8> {
        let bytes = Self::bytes(entries, path);
        let Some(raw_name) = bytes.get(0x40..0x60) else {
            return Vec::new();
        };
        let end = raw_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(raw_name.len());
        if raw_name[..end] == *name.as_bytes() {
            bytes
        } else {
            Vec::new()
        }
    }

    fn file_order(entries: &BTreeMap<String, Entry>, root: &str, name: &str) -> u64 {
        let index = Self::bytes(entries, &format!("{root}/_pcsx2_index"));
        serde_json::from_slice::<serde_json::Value>(&index)
            .ok()
            .and_then(|index| index.get(name)?.get("order")?.as_u64())
            .unwrap_or(u64::MAX)
    }

    fn saves(entries: &Arc<Mutex<BTreeMap<String, Entry>>>) -> Result<Vec<Save<'static>>, Error> {
        let entries = entries.lock();
        let roots = entries
            .iter()
            .filter_map(|(path, entry)| {
                (!path.contains('/')
                    && !Self::is_private(path)
                    && matches!(entry, Entry::Directory { .. }))
                .then_some(path.clone())
            })
            .collect::<Vec<_>>();
        let mut saves = Vec::new();
        for name in roots {
            let prefix = format!("{name}/");
            let mut files = entries
                .iter()
                .filter_map(|(path, entry)| {
                    let child = path.strip_prefix(&prefix)?;
                    (!child.contains('/')
                        && !Self::is_private(child)
                        && matches!(entry, Entry::File { .. }))
                    .then(|| {
                        (
                            child.to_owned(),
                            Self::dirent(&entries, &format!("{name}/_pcsx2_meta/{child}"), child),
                            Self::bytes(&entries, path),
                        )
                    })
                })
                .collect::<Vec<_>>();
            if files.is_empty() {
                continue;
            }
            files.sort_by_key(|file| (Self::file_order(&entries, &name, &file.0), file.0.clone()));
            let dirent = Self::dirent(&entries, &format!("{name}/_pcsx2_meta_directory"), &name);
            saves.push(
                Save::from_files(
                    &name,
                    &dirent,
                    files.iter().map(|(name, dirent, data)| {
                        (name.as_str(), dirent.as_slice(), data.as_slice())
                    }),
                )
                .map_err(|error| Error::MalformedMetadata {
                    file: name,
                    reason: error.to_string(),
                })?,
            );
        }
        Ok(saves)
    }

    fn handle(&self, path: &str, position: u64) -> Result<Handle, Error> {
        let Entry::File { bytes, mtime } = self.entry(path)? else {
            return Err(Error::Unsupported(format!("`{path}` is not a file")));
        };
        Ok(Handle {
            bytes,
            mtime,
            position,
            worker: self.worker.clone(),
            writeback: path.contains('/'),
        })
    }

    fn remove_tree(entries: &mut BTreeMap<String, Entry>, path: &str) {
        let prefix = format!("{path}/");
        entries.retain(|entry, _| entry != path && !entry.starts_with(&prefix));
    }
}

impl fmt::Debug for Pcsx2MemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pcsx2MemoryStorage")
            .field("backing", &self.backing)
            .finish_non_exhaustive()
    }
}

impl Storage for Pcsx2MemoryStorage {
    fn exists(&self, path: &str) -> bool {
        path.is_empty() || self.entries.lock().contains_key(path)
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
        if recursive {
            return ReadDir::recursive(self, path);
        }
        if !path.is_empty() && self.metadata(path)?.kind != EntryKind::Directory {
            return Err(Error::Unsupported(format!("`{path}` is not a directory")));
        }
        let mut entries = self.children(path);
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(ReadDir::surface(entries.into_iter()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        let mut entries = self.entries.lock();
        let mut prefix = String::new();
        for component in path.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            entries
                .entry(prefix.clone())
                .or_insert_with(Entry::directory);
        }
        drop(entries);
        self.worker.schedule();
        Ok(())
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
        let entry = {
            let mut entries = self.entries.lock();
            Self::create_parents(&mut entries, path);
            let entry = Entry::file(Vec::new());
            entries.insert(path.to_owned(), entry.clone());
            entry
        };
        self.worker.schedule();
        let Entry::File { bytes, mtime } = entry else {
            unreachable!()
        };
        Ok(Box::new(Handle {
            bytes,
            mtime,
            position: 0,
            worker: self.worker.clone(),
            writeback: path.contains('/'),
        }))
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        Ok(Box::new(self.handle(path, 0)?))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        let Entry::File { bytes, mtime } = self.entry(path)? else {
            return Err(Error::Unsupported(format!("`{path}` is not a file")));
        };
        Ok(Arc::new(Pcsx2StorageFile {
            bytes,
            mtime,
            worker: self.worker.clone(),
            writable: write,
            writeback: path.contains('/'),
        }))
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
        if !self.exists(path) {
            return self.create(path);
        }
        Ok(Box::new(self.handle(path, self.metadata(path)?.len)?))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        let mut entries = self.entries.lock();
        let prefix = format!("{path}/");
        if entries.keys().any(|entry| entry.starts_with(&prefix)) {
            return Err(Error::Unsupported(format!(
                "directory `{path}` is not empty"
            )));
        }
        entries.remove(path);
        drop(entries);
        self.worker.schedule();
        Ok(())
    }

    fn watch(&self, _paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        Ok(None)
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

impl MountableStorage for Pcsx2MemoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let mut entries = self.entries.lock();
        if to
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with(DELETED_PREFIX))
        {
            Self::remove_tree(&mut entries, from);
            drop(entries);
            self.worker.schedule();
            return Ok(());
        }
        let source = entries.remove(from).ok_or_else(|| Error::not_found(from))?;
        let old_prefix = format!("{from}/");
        let descendants = entries
            .keys()
            .filter(|path| path.starts_with(&old_prefix))
            .cloned()
            .collect::<Vec<_>>();
        Self::remove_tree(&mut entries, to);
        Self::create_parents(&mut entries, to);
        entries.insert(to.to_owned(), source);
        for old in descendants {
            let entry = entries.remove(&old).expect("listed descendant");
            entries.insert(format!("{to}/{}", &old[old_prefix.len()..]), entry);
        }
        drop(entries);
        self.worker.schedule();
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        let Entry::File { bytes, mtime } = self.entry(path)? else {
            return Err(Error::Unsupported(format!("`{path}` is not a file")));
        };
        bytes.lock().resize(
            usize::try_from(len)
                .map_err(|_| Error::Unsupported("PCSX2 file is too large".to_owned()))?,
            0,
        );
        *mtime.lock() = Timestamp::now();
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
    bytes: Arc<Mutex<Vec<u8>>>,
    mtime: Arc<Mutex<Timestamp>>,
    position: u64,
    worker: ReconcileWorker,
    writeback: bool,
}

impl Handle {
    fn commit(&self) -> Result<(), Error> {
        if !self.writeback {
            return Ok(());
        }
        self.worker.schedule();
        Ok(())
    }
}

struct Pcsx2StorageFile {
    bytes: Arc<Mutex<Vec<u8>>>,
    mtime: Arc<Mutex<Timestamp>>,
    worker: ReconcileWorker,
    writable: bool,
    writeback: bool,
}

impl StorageFile for Pcsx2StorageFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        Ok(Metadata {
            kind: EntryKind::File,
            len: self.bytes.lock().len() as u64,
            mtime: *self.mtime.lock(),
        })
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
        let bytes = self.bytes.lock();
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let copied = output.len().min(bytes.len() - start);
        output[..copied].copy_from_slice(&bytes[start..start + copied]);
        Ok(copied)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, Error> {
        if !self.writable {
            return Err(Error::Unsupported(
                "PCSX2 file handle is read-only".to_owned(),
            ));
        }
        let start = usize::try_from(offset)
            .map_err(|_| Error::Unsupported("PCSX2 file offset is too large".to_owned()))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| Error::Unsupported("PCSX2 file write is too large".to_owned()))?;
        let mut bytes = self.bytes.lock();
        if end > bytes.len() {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(input);
        drop(bytes);
        *self.mtime.lock() = Timestamp::now();
        if self.writeback {
            self.worker.schedule();
        }
        Ok(input.len())
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::Unsupported(
                "PCSX2 file handle is read-only".to_owned(),
            ));
        }
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("PCSX2 file is too large".to_owned()))?;
        self.bytes.lock().resize(len, 0);
        *self.mtime.lock() = Timestamp::now();
        if self.writeback {
            self.worker.schedule();
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        if self.writeback {
            self.worker.schedule();
        }
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.worker.reconcile()
    }
}

impl Read for Handle {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let bytes = self.bytes.lock();
        let start = usize::try_from(self.position)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let copied = output.len().min(bytes.len() - start);
        output[..copied].copy_from_slice(&bytes[start..start + copied]);
        self.position += copied as u64;
        Ok(copied)
    }
}

impl Write for Handle {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let start = usize::try_from(self.position)
            .map_err(|_| std::io::Error::other("PCSX2 file offset is too large"))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| std::io::Error::other("PCSX2 file write is too large"))?;
        {
            let mut bytes = self.bytes.lock();
            if end > bytes.len() {
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(input);
        }
        self.position = end as u64;
        *self.mtime.lock() = Timestamp::now();
        self.commit().map_err(std::io::Error::other)?;
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.writeback {
            self.worker.reconcile().map_err(std::io::Error::other)
        } else {
            Ok(())
        }
    }
}

impl Seek for Handle {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let len = self.bytes.lock().len() as i128;
        let position = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => len + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(u64::MAX)).contains(&position) {
            return Err(std::io::Error::other("invalid PCSX2 file seek"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}
