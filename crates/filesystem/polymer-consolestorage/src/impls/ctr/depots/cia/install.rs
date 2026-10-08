use super::CiaDepot;
use super::metadata::{InstalledTitle, TmdHeader};
use super::ncch::NcchDecryptor;
use crate::{
    Error, Storage, VolumeStorage,
    installable::{DepotFile, InstallContext, Installable, StorageWriter, artifacts::Artifact},
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
    storage: Arc<dyn Storage>,
    seed: Option<[u8; 16]>,
}

impl CiaInstall {
    pub async fn new(
        depot: &CiaDepot,
        current: Arc<dyn VolumeStorage>,
        storage: Arc<dyn Storage>,
        seed: Option<[u8; 16]>,
        context: &InstallContext,
    ) -> Result<Self, Error> {
        let depot = depot.clone();
        let old = current
            .artifacts()
            .artifact(&format!("{:016X}", depot.title_id))
            .await?;
        context
            .spawn_blocking(move |context| {
                let archive = CiaArchive::open(&depot.path)
                    .map_err(|e| Error::malformed("CIA", e.to_string()))?;
                let entries = depot.jobs(&archive);
                context.phase(
                    crate::installable::InstallPhase::Installing,
                    archive.items().iter().map(|item| item.size).sum(),
                );
                let retained = Self::retain_contents(&depot, &archive, &*current, old.as_ref())?;
                for file in &retained {
                    let path = file.path().to_string_lossy();
                    let mut writer = StorageWriter {
                        file: storage.create(&path)?,
                        position: 0,
                        hash: blake3::Hasher::new(),
                    };
                    let mut reader = crate::installable::Reporting {
                        inner: current.open_read(&path)?,
                        context: context.clone(),
                    };
                    std::io::copy(&mut reader, &mut writer)
                        .map_err(|error| Error::io(file.path(), error))?;
                    writer
                        .flush()
                        .map_err(|error| Error::io(file.path(), error))?;
                    if writer.position != file.byte_length()
                        || writer.hash.finalize().as_bytes() != file.blake3()
                    {
                        return Err(Error::HistoryConflict(format!(
                            "retained content changed: {path}"
                        )));
                    }
                }
                Ok(Self {
                    archive: Arc::new(Mutex::new(archive)),
                    entries: entries.into_iter(),
                    retained,
                    depot: depot.clone(),
                    storage,
                    seed,
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
        storage: &dyn Storage,
        old: Option<&Artifact>,
    ) -> Result<Vec<DepotFile>, Error> {
        let mut retained = Vec::new();
        if depot.kind != crate::installable::DepotType::AppContent {
            return Ok(retained);
        }
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
        storage: &dyn Storage,
        entry: Entry,
        seed: Option<[u8; 16]>,
        context: &InstallContext,
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
                        context.advance(bytes);
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
    async fn next_batch(
        &mut self,
        context: &InstallContext,
    ) -> Result<Option<Vec<DepotFile>>, Error> {
        let Some(entry) = self.entries.next() else {
            return Ok(None);
        };
        let archive = self.archive.clone();
        let storage = self.storage.clone();
        let seed = self.seed;
        context
            .spawn_blocking(move |context| {
                let mut archive = archive.lock();
                let file = Self::write_entry(&mut archive, &*storage, entry, seed, context)?;
                Ok(Some(vec![file]))
            })
            .await
            .map_err(|e| Error::Unsupported(format!("CIA worker failed: {e}")))?
    }

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let manifest = DepotFile::merge(self.retained, manifest);
        InstalledTitle::read(&self.depot, &*self.storage, manifest)
    }
}
