use crate::installable::{InstallContext, Installable};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use pkgrip::PkgArchive;

use super::Ps3PkgDepot;
use crate::installable::DepotFile;
use crate::installable::DepotType;
use crate::installable::StorageWriter;
use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};
use crate::{Error, VolumeStorage};

pub(super) struct Ps3PkgInstall {
    extraction: Arc<Mutex<PkgExtraction>>,
    depot: Ps3PkgDepot,
    retained: Vec<DepotFile>,
    storage: Arc<dyn VolumeStorage>,
}

struct PkgExtraction {
    archive: PkgArchive<File>,
    root: String,
    manifest: Vec<DepotFile>,
    index: usize,
    storage: Arc<dyn VolumeStorage>,
}

impl Ps3PkgInstall {
    pub async fn new(
        depot: &Ps3PkgDepot,
        storage: Arc<dyn VolumeStorage>,
        context: &InstallContext,
    ) -> Result<Self, Error> {
        let depot = depot.clone();
        let mut install = context
            .spawn_blocking(move |context| {
                let file = depot.verified_file(context)?;
                let size = file
                    .metadata()
                    .map_err(|error| Error::io(&depot.path, error))?
                    .len();
                let archive = PkgArchive::from_reader(file, size)
                    .map_err(|error| Error::io(&depot.path, error))?;
                context.phase(
                    crate::installable::InstallPhase::Installing,
                    archive
                        .items()
                        .iter()
                        .filter(|item| item.is_file)
                        .map(|item| item.file_size)
                        .sum(),
                );
                Ok::<_, Error>(Self {
                    extraction: Arc::new(Mutex::new(PkgExtraction {
                        archive,
                        root: depot.install_path.clone(),
                        manifest: Vec::new(),
                        index: 0,
                        storage: storage.clone(),
                    })),
                    depot,
                    retained: Vec::new(),
                    storage,
                })
            })
            .await
            .map_err(|error| Error::Unsupported(format!("PKG worker failed: {error}")))??;
        // Reinstalling a patch is additive. Retain ownership of earlier files.
        let id = install.depot.artifact_id();
        if let Some(old) = install.storage.artifacts().artifact(&id).await? {
            install.retained = old.manifest().to_vec();
        }
        Ok(install)
    }
}

impl PkgExtraction {
    fn record_file(&mut self, path: String, mut writer: StorageWriter) -> Result<(), Error> {
        writer
            .flush()
            .map_err(|error| Error::io(&PathBuf::from(&path), error))?;
        self.manifest
            .retain(|file| file.path() != std::path::Path::new(&path));
        self.manifest.push(DepotFile::new(
            PathBuf::from(path),
            *writer.hash.finalize().as_bytes(),
            writer.position,
        ));
        Ok(())
    }
}

impl PkgExtraction {
    fn next_file(&mut self, context: &InstallContext) -> Result<String, Error> {
        let item = &self.archive.items()[self.index];
        let path = format!("{}/{}", self.root, item.name);
        if item.is_file {
            let mut writer = StorageWriter {
                file: self.storage.create(&path)?,
                position: 0,
                hash: blake3::Hasher::new(),
            };
            self.archive
                .stream_item_with_progress(self.index, &mut writer, |bytes| context.advance(bytes))
                .map_err(|error| Error::io(&PathBuf::from(&path), error))?;
            self.record_file(path.clone(), writer)?;
        } else {
            self.storage.create_dir_all(&path)?;
        }
        self.index += 1;
        Ok(path)
    }
}

impl Installable for Ps3PkgInstall {
    async fn next_batch(
        &mut self,
        context: &InstallContext,
    ) -> Result<Option<Vec<DepotFile>>, Error> {
        let extraction = self.extraction.clone();
        context
            .spawn_blocking(move |context| {
                let mut extraction = extraction.lock();
                if extraction.index == extraction.archive.items().len() {
                    return Ok(None);
                }
                extraction.next_file(context)?;
                Ok(Some(std::mem::take(&mut extraction.manifest)))
            })
            .await
            .map_err(|error| Error::Unsupported(format!("PKG worker failed: {error}")))?
    }
    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let root = &self.depot.install_path;
        let path = format!("{root}/PARAM.SFO");
        let sfo = if self.storage.exists(&path) {
            crate::impls::sony::metadata::SonyMetadata::read(self.storage.open_read(&path)?, &path)?
        } else {
            orbis_unpkg::Psf::default()
        };
        if sfo
            .get_string("TITLE_ID")
            .is_some_and(|id| id != self.depot.title_id)
        {
            return Err(Error::malformed(path, "installed PS3 identity changed"));
        }
        let icon = crate::installable::IconHint::read(
            &*self.storage,
            &format!("{root}/ICON0.PNG"),
            self.depot.kind,
        )?;
        Artifact::new(
            self.depot.artifact_id(),
            self.depot.kind,
            DepotFile::merge(self.retained, manifest),
            crate::impls::sony::metadata::SonyMetadata::title(&sfo)
                .unwrap_or(&self.depot.title_id)
                .to_owned(),
            sfo.get_string("APP_VER")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .or(self.depot.version),
            vec![icon],
            (self.depot.kind == DepotType::App).then(|| ArtifactEntrypoint {
                root: root.clone(),
                path: "USRDIR/EBOOT.BIN".to_owned(),
                package: root.clone(),
            }),
        )
    }
}
