use super::{Ps3PupDepot, sce::Sce};
use crate::installable::DepotFile;
use crate::installable::StorageWriter;
use crate::installable::artifacts::Artifact;
use crate::installable::{InstallContext, Installable};
use crate::{Error, Storage};
use parking_lot::Mutex;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

pub(super) struct PupInstall {
    extraction: Arc<Mutex<PupExtraction>>,
    version: String,
    storage: Arc<dyn Storage>,
}

struct PupExtraction {
    file: File,
    packages: Vec<(String, u64, u64)>,
    manifest: Vec<DepotFile>,
    paths: std::collections::BTreeSet<String>,
    index: usize,
    storage: Arc<dyn Storage>,
}
impl PupInstall {
    pub async fn new(
        depot: &Ps3PupDepot,
        storage: Arc<dyn Storage>,
        context: &InstallContext,
    ) -> Result<Self, Error> {
        let depot = depot.clone();
        context
            .spawn_blocking(move |context| {
                let mut pup = depot.verified_pup(context)?;
                let (offset, size) = pup.entry(0x300).map_err(|e| Error::io(&depot.path, e))?;
                let mut archive = tar::Archive::new((&mut pup.file).take(size));
                let mut packages = Vec::new();
                for entry in archive.entries().map_err(|e| Error::io(&depot.path, e))? {
                    let entry = entry.map_err(|e| Error::io(&depot.path, e))?;
                    let name = entry
                        .path()
                        .map_err(|e| Error::io(&depot.path, e))?
                        .to_string_lossy()
                        .into_owned();
                    if !name.starts_with("dev_flash_") {
                        continue;
                    }
                    if !entry.header().entry_type().is_file() || entry.size() > Sce::MAX_SIZE {
                        return Err(Error::malformed("PUP", "invalid flash package"));
                    }
                    if entry
                        .raw_file_position()
                        .checked_add(entry.size())
                        .is_none_or(|end| end > size)
                    {
                        return Err(Error::malformed("PUP", "flash package exceeds update TAR"));
                    }
                    packages.push((name, offset + entry.raw_file_position(), entry.size()));
                }
                if packages.is_empty() {
                    return Err(Error::malformed("PUP", "no flash packages"));
                }
                context.phase(
                    crate::installable::InstallPhase::Installing,
                    packages.iter().map(|(_, _, size)| size).sum(),
                );
                Ok(Self {
                    extraction: Arc::new(Mutex::new(PupExtraction {
                        file: pup.file,
                        packages,
                        manifest: Vec::new(),
                        paths: std::collections::BTreeSet::new(),
                        index: 0,
                        storage: storage.clone(),
                    })),
                    version: depot.version,
                    storage,
                })
            })
            .await
            .map_err(|e| Error::Unsupported(format!("PUP worker failed: {e}")))?
    }
}

impl PupExtraction {
    fn extract(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut archive = tar::Archive::new(bytes);
        for entry in archive
            .entries()
            .map_err(|e| Error::malformed("flash TAR", e.to_string()))?
        {
            let mut entry = entry.map_err(|e| Error::malformed("flash TAR", e.to_string()))?;
            let path = entry
                .path()
                .map_err(|e| Error::malformed("flash TAR", e.to_string()))?
                .to_str()
                .ok_or_else(|| Error::malformed("flash TAR", "non-UTF-8 path"))?
                .trim_end_matches('/')
                .to_owned();
            super::super::pkg::Ps3PkgDepot::validate_path(&path)?;
            if path != "dev_flash" && !path.starts_with("dev_flash/") {
                return Err(Error::InvalidPath(path));
            }
            let kind = entry.header().entry_type();
            if kind.is_dir() {
                self.storage.create_dir_all(&path)?;
                continue;
            }
            if !kind.is_file() {
                return Err(Error::malformed("flash TAR", "unsupported entry type"));
            }
            if !self.paths.insert(path.to_lowercase()) {
                return Err(Error::malformed("flash TAR", "duplicate firmware file"));
            }
            let mut writer = StorageWriter {
                file: self.storage.create(&path)?,
                position: 0,
                hash: blake3::Hasher::new(),
            };
            io::copy(&mut entry, &mut writer)
                .and_then(|_| writer.flush())
                .map_err(|e| Error::io(&PathBuf::from(&path), e))?;
            if writer.position != entry.size() {
                return Err(Error::malformed("flash TAR", "truncated firmware file"));
            }
            self.manifest.push(DepotFile::new(
                PathBuf::from(path),
                *writer.hash.finalize().as_bytes(),
                writer.position,
            ));
        }
        Ok(())
    }
}

impl PupExtraction {
    fn next_file(&mut self, context: &InstallContext) -> Result<String, Error> {
        let (name, offset, size) = &self.packages[self.index];
        let path = PathBuf::from(name);
        self.file
            .seek(SeekFrom::Start(*offset))
            .map_err(|e| Error::io(&path, e))?;
        let mut bytes = vec![0; *size as usize];
        self.file
            .read_exact(&mut bytes)
            .map_err(|e| Error::io(&path, e))?;
        let bytes = Sce::flash_tar(&bytes).map_err(|e| Error::io(&path, e))?;
        self.extract(&bytes)?;
        let (name, _, size) = self.packages[self.index].clone();
        context.advance(size);
        self.index += 1;
        Ok(name)
    }
}

impl Installable for PupInstall {
    async fn next_batch(
        &mut self,
        context: &InstallContext,
    ) -> Result<Option<Vec<DepotFile>>, Error> {
        let extraction = self.extraction.clone();
        context
            .spawn_blocking(move |context| {
                let mut extraction = extraction.lock();
                if extraction.index == extraction.packages.len() {
                    return Ok(None);
                }
                extraction.next_file(context)?;
                Ok(Some(std::mem::take(&mut extraction.manifest)))
            })
            .await
            .map_err(|error| Error::Unsupported(format!("PUP worker failed: {error}")))?
    }
    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        if manifest.is_empty() {
            return Err(Error::malformed("PUP", "empty firmware image"));
        }
        let version_path = "dev_flash/vsh/etc/version.txt";
        let version = if manifest
            .iter()
            .any(|file| file.path() == std::path::Path::new(version_path))
        {
            let mut bytes = Vec::new();
            self.storage
                .open_read(version_path)?
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .map_err(|e| Error::io(std::path::Path::new(version_path), e))?;
            String::from_utf8_lossy(&bytes)
                .lines()
                .find_map(|line| {
                    line.strip_prefix("release:").map(|value| {
                        value
                            .split(':')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_owned()
                    })
                })
                .filter(|value| !value.is_empty())
                .unwrap_or(self.version)
        } else {
            self.version
        };
        let artifact = Artifact::new(
            "ps3:firmware".into(),
            crate::installable::DepotType::System,
            manifest,
            "PlayStation 3 system software".into(),
            Some(version),
            vec![crate::installable::IconHint::Builtin(
                crate::installable::DepotType::System,
            )],
            None,
        )?;
        Ok(artifact)
    }
}
