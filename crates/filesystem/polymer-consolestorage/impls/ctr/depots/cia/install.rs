use super::CiaDepot;
use crate::{
    Error, VolumeStorage,
    installable::{
        DepotFile, DepotType, Installable, StorageWriter,
        artifacts::{Artifact, DescribeArtifact},
    },
};
use csis::{CiaArchive, CiaSection};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
};

pub(in crate::impls::ctr) enum Content {
    Item(usize),
    Section(CiaSection),
}
pub(in crate::impls::ctr) struct Entry {
    pub path: String,
    pub content: Content,
}

pub(in crate::impls::ctr) struct CiaInstall {
    archive: CiaArchive<File>,
    entries: std::vec::IntoIter<Entry>,
    artifact: Artifact,
    old_paths: Vec<String>,
    root: String,
    storage: Arc<dyn VolumeStorage>,
}

impl CiaInstall {
    pub async fn new(depot: &CiaDepot, storage: Arc<dyn VolumeStorage>) -> Result<Self, Error> {
        let depot = depot.clone();
        let artifact = depot.describe();
        let old = storage.artifacts().artifact(artifact.id()).await?;
        tokio::task::spawn_blocking(move || {
            let archive = CiaArchive::open(&depot.path)
                .map_err(|e| Error::malformed("CIA", e.to_string()))?;
            let entries = depot.jobs(&archive);
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
                archive,
                entries: entries.into_iter(),
                artifact: artifact.with_manifest(retained),
                old_paths,
                root: depot.root.clone(),
                storage,
            })
        })
        .await
        .map_err(|e| Error::Unsupported(format!("CIA worker failed: {e}")))?
    }

    /// Retain already installed DLC omitted by a partial CIA only if its new TMD
    /// still describes the same bytes. Never retain an obsolete version blindly.
    fn retain_contents(
        depot: &CiaDepot,
        archive: &CiaArchive<File>,
        storage: &dyn VolumeStorage,
        old: Option<&Artifact>,
    ) -> Result<Vec<DepotFile>, Error> {
        let mut retained = Vec::new();
        let Some(old) = old.filter(|_| depot.kind != DepotType::AppTicket) else {
            return Ok(retained);
        };
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
            if Self::matches_content(storage, &path, record)? {
                retained.push(DepotFile::new(
                    file.path().to_owned(),
                    *file.blake3(),
                    file.byte_length(),
                ));
            }
        }
        Ok(retained)
    }

    fn matches_content(
        storage: &dyn VolumeStorage,
        path: &str,
        record: &csis::ContentRecord,
    ) -> Result<bool, Error> {
        if !storage.exists(path) || storage.metadata(path)?.len != record.size {
            return Ok(false);
        }
        let mut reader = storage.open_read(path)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|e| Error::io(&PathBuf::from(path), e))?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        Ok(hash.finalize().as_slice() == record.sha256)
    }

    fn write_entry(&mut self, entry: Entry) -> Result<DepotFile, Error> {
        let mut writer = StorageWriter {
            file: self.storage.create(&entry.path)?,
            position: 0,
            hash: blake3::Hasher::new(),
        };
        match entry.content {
            Content::Item(index) => self.archive.stream_item(index, &mut writer),
            Content::Section(section) => self.archive.stream_section(section, &mut writer),
        }
        .map_err(|e| Error::malformed("CIA", e.to_string()))?;
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
    fn artifact(&self) -> &Artifact {
        &self.artifact
    }
    async fn next_batch(mut self) -> Result<(Self, Option<Vec<DepotFile>>), Error> {
        let Some(entry) = self.entries.next() else {
            return Ok((self, None));
        };
        tokio::task::spawn_blocking(move || {
            let file = self.write_entry(entry)?;
            Ok((self, Some(vec![file])))
        })
        .await
        .map_err(|e| Error::Unsupported(format!("CIA worker failed: {e}")))?
    }
    async fn finish(self, artifact: Artifact) -> Result<Artifact, Error> {
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
        self.storage.artifacts().record(&artifact).await?;
        Ok(artifact)
    }
}
