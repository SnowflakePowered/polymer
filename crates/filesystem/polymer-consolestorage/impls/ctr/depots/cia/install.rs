use super::CiaDepot;
use super::metadata::{InstalledTitle, TmdHeader};
use super::ncch::NcchDecryptor;
use crate::{
    Error, VolumeStorage,
    installable::{DepotFile, InstallEvents, Installable, StorageWriter, artifacts::Artifact},
};
use csis::{CiaArchive, CiaSection};
use parking_lot::Mutex;
use std::{fs::File, io::Write, path::PathBuf, sync::Arc};

pub(in crate::impls::ctr) enum Content {
    Item(usize),
    Section(CiaSection),
}
pub(in crate::impls::ctr) struct Entry {
    pub path: String,
    pub content: Content,
}

pub(in crate::impls::ctr) struct CiaInstall {
    archive: Arc<Mutex<CiaArchive<File>>>,
    entries: std::vec::IntoIter<Entry>,
    depot: CiaDepot,
    retained: Vec<DepotFile>,
    old_paths: Vec<String>,
    root: String,
    storage: Arc<dyn VolumeStorage>,
    seed: Option<[u8; 16]>,
    events: InstallEvents,
}

impl CiaInstall {
    pub async fn new(
        depot: &CiaDepot,
        storage: Arc<dyn VolumeStorage>,
        seed: Option<[u8; 16]>,
        events: InstallEvents,
    ) -> Result<Self, Error> {
        let depot = depot.clone();
        let old = storage
            .artifacts()
            .artifact(&format!("{:016X}", depot.title_id))
            .await?;
        tokio::task::spawn_blocking(move || {
            let archive = CiaArchive::open(&depot.path)
                .map_err(|e| Error::malformed("CIA", e.to_string()))?;
            let entries = depot.jobs(&archive);
            events.phase(
                crate::installable::InstallPhase::Installing,
                archive.items().iter().map(|item| item.size).sum(),
            );
            let retained = Self::retain_contents(&depot, &archive, &*storage, old.as_ref())?;
            let old_paths = old
                .as_ref()
                .map(|a| {
                    a.manifest()
                        .iter()
                        .map(|f| f.path().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            Ok(Self {
                archive: Arc::new(Mutex::new(archive)),
                entries: entries.into_iter(),
                retained,
                depot: depot.clone(),
                old_paths,
                root: depot.root.clone(),
                storage,
                seed,
                events,
            })
        })
        .await
        .map_err(|e| Error::Unsupported(format!("CIA worker failed: {e}")))?
    }

    /// Retain already installed DLC omitted by a partial CIA only if its new TMD
    /// still describes the same content. Installed contents are decrypted, so they
    /// are matched against the installed TMD rather than hashed.
    fn retain_contents(
        depot: &CiaDepot,
        archive: &CiaArchive<File>,
        storage: &dyn VolumeStorage,
        old: Option<&Artifact>,
    ) -> Result<Vec<DepotFile>, Error> {
        let mut retained = Vec::new();
        let Some(old) = old else {
            return Ok(retained);
        };
        let tmd = InstalledTitle::tmd(depot, storage)?;
        let (_, installed) = TmdHeader::parse(&tmd)?;
        for record in archive.tmd().contents() {
            if archive
                .items()
                .iter()
                .any(|item| item.content_index == record.content_index)
            {
                continue;
            }
            let path = depot.content_path(record.content_id);
            let Some(file) = old
                .manifest()
                .iter()
                .find(|file| file.path() == std::path::Path::new(&path))
            else {
                continue;
            };
            let unchanged = installed.iter().any(|content| {
                content.id.get() == record.content_id
                    && content.size.get() == record.size
                    && content.hash == record.sha256
            });
            if unchanged && storage.exists(&path) && storage.metadata(&path)?.len == record.size {
                retained.push(DepotFile::new(
                    file.path().to_owned(),
                    *file.blake3(),
                    file.byte_length(),
                ));
            }
        }
        Ok(retained)
    }

    fn write_entry(
        archive: &mut CiaArchive<File>,
        storage: &dyn VolumeStorage,
        entry: Entry,
        seed: Option<[u8; 16]>,
        events: &InstallEvents,
    ) -> Result<DepotFile, Error> {
        let mut writer = StorageWriter {
            file: storage.create(&entry.path)?,
            position: 0,
            hash: blake3::Hasher::new(),
        };
        match entry.content {
            Content::Item(index) => {
                let mut ncch = NcchDecryptor::new(&mut writer, seed);
                archive
                    .stream_item_with_progress(index, &mut ncch, |bytes| {
                        events.advance(bytes);
                        Ok(())
                    })
                    .map_err(|e| Error::malformed("CIA", e.to_string()))?;
                ncch.finish()
                    .map_err(|e| Error::io(&PathBuf::from(&entry.path), e))?;
            }
            Content::Section(section) => {
                archive
                    .stream_section(section, &mut writer)
                    .map_err(|e| Error::malformed("CIA", e.to_string()))?;
            }
        }
        writer
            .flush()
            .map_err(|e| Error::io(&PathBuf::from(&entry.path), e))?;
        Ok(DepotFile::new(
            PathBuf::from(entry.path),
            *writer.hash.finalize().as_bytes(),
            writer.position,
        ))
    }
}
impl Installable for CiaInstall {
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error> {
        let Some(entry) = self.entries.next() else {
            return Ok(None);
        };
        let archive = self.archive.clone();
        let storage = self.storage.clone();
        let events = self.events.clone();
        let seed = self.seed;
        tokio::task::spawn_blocking(move || {
            let mut archive = archive.lock();
            let file = Self::write_entry(&mut archive, &*storage, entry, seed, &events)?;
            Ok(Some(vec![file]))
        })
        .await
        .map_err(|e| Error::Unsupported(format!("CIA worker failed: {e}")))?
    }

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let manifest = DepotFile::merge(self.retained, manifest);
        let artifact = InstalledTitle::read(&self.depot, &*self.storage, manifest)?;
        for path in self.old_paths {
            if path.starts_with(&format!("{}/", self.root))
                && !artifact
                    .manifest()
                    .iter()
                    .any(|f| f.path() == std::path::Path::new(&path))
            {
                self.storage.remove(&path)?;
            }
        }
        Ok(artifact)
    }
}
