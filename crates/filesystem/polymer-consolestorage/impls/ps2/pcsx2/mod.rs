use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;
use tokio::io::AsyncReadExt;

use crate::impls::memorycard::{MemoryFile, ReconcileWorker};
use crate::{
    BoxFuture, DirEntry, EntryKind, Error, InstallResult, Metadata, MountableStorage, ReadDir,
    Storage, StorageFile,
};

use super::psu::Save;
use super::raw;

const SUPERBLOCK: &str = "_pcsx2_superblock";
const DELETED_PREFIX: &str = "_pcsx2_deleted_";

type Reconcile = dyn Fn(&[Save<'static>]) -> Result<(), Error> + Send + Sync;

#[derive(Clone)]
enum Entry {
    Directory { mtime: Arc<Mutex<Timestamp>> },
    File { file: MemoryFile },
}

impl Entry {
    fn directory() -> Self {
        Self::Directory {
            mtime: Arc::new(Mutex::new(Timestamp::now())),
        }
    }

    fn file(bytes: Vec<u8>) -> Self {
        Self::File {
            file: MemoryFile::new(bytes),
        }
    }

    fn metadata(&self) -> Metadata {
        match self {
            Self::Directory { mtime } => Metadata {
                kind: EntryKind::Directory,
                len: 0,
                mtime: *mtime.lock(),
            },
            Self::File { file } => file.metadata(),
        }
    }
}

/// An in-memory PCSX2 folder-card projection of PSU-backed saves.
pub(super) struct Pcsx2MemoryStorage {
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    worker: ReconcileWorker,
}

impl Pcsx2MemoryStorage {
    pub(super) fn new<'a>(
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
        let worker =
            ReconcileWorker::spawn("polymer-pcsx2-writeback", "PCSX2 memory card", move || {
                background_reconcile(&Self::saves(&background_entries)?)
            })?;
        Ok(Self { entries, worker })
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
            Some(Entry::File { file }) => file.bytes(),
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

    fn remove_tree(entries: &mut BTreeMap<String, Entry>, path: &str) {
        let prefix = format!("{path}/");
        entries.retain(|entry, _| entry != path && !entry.starts_with(&prefix));
    }
}

impl fmt::Debug for Pcsx2MemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pcsx2MemoryStorage")
            .field("entries", &self.entries.lock().len())
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

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        let entry = {
            let mut entries = self.entries.lock();
            Self::create_parents(&mut entries, path);
            let entry = Entry::file(Vec::new());
            entries.insert(path.to_owned(), entry.clone());
            entry
        };
        self.worker.schedule();
        let Entry::File { file } = entry else {
            unreachable!()
        };
        Ok(file.handle(self.worker.clone(), true, path.contains('/')))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        let Entry::File { file } = self.entry(path)? else {
            return Err(Error::Unsupported(format!("`{path}` is not a file")));
        };
        Ok(file.handle(self.worker.clone(), write, path.contains('/')))
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
            self.create(&dest)?.write_all_at(0, &bytes)?;
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
        let Entry::File { file } = self.entry(path)? else {
            return Err(Error::Unsupported(format!("`{path}` is not a file")));
        };
        file.handle(self.worker.clone(), true, path.contains('/'))
            .set_len(len)
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
