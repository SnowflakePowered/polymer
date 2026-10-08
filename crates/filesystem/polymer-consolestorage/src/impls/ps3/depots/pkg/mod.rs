mod install;
mod metadata;

use crate::installable::{IconHint, InstallContext, InstallPreview, Reporting};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use orbis_unpkg::Psf;
use pkgrip::PkgArchive;
use sha1::{Digest, Sha1};

use super::super::composer::PkgContribution;
use super::super::{Ps3Nand, TitleId};
use crate::Error;
use crate::installable::{Depot, DepotType, Installable};
use crate::nand::VirtualNand;

/// A retail PS3 game, update, or additional-content PKG.
/// Decryption support is provided by pkgrip.
#[derive(Clone)]
pub struct Ps3PkgDepot {
    path: PathBuf,
    title_id: String,
    content_id: String,
    install_path: String,
    kind: DepotType,
    version: Option<String>,
    digest: [u8; 20],
    label: Option<String>,
    icon: Option<Vec<u8>>,
    contribution: PkgContribution,
}

impl Ps3PkgDepot {
    pub fn title_id(&self) -> &str {
        &self.title_id
    }
    pub fn content_id(&self) -> &str {
        &self.content_id
    }
    pub fn install_path(&self) -> &str {
        &self.install_path
    }

    pub(super) fn validate_path(path: &str) -> Result<(), Error> {
        if path.split('/').any(|part| {
            matches!(part, "" | "." | "..")
                || part.eq_ignore_ascii_case(".git")
                || part.ends_with(['.', ' '])
                || part.contains(['\\', ':', '\0', '<', '>', '"', '|', '?', '*'])
        }) {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        Ok(())
    }

    fn validate_entries(archive: &PkgArchive<File>) -> Result<(), Error> {
        let mut paths = BTreeMap::new();
        for item in archive.items() {
            Self::validate_path(&item.name)?;
            if item.is_psp() {
                return Err(Error::Unsupported("PSP entries in a PS3 PKG".to_owned()));
            }
            if paths
                .insert(item.name.to_lowercase(), item.is_file)
                .is_some()
            {
                return Err(Error::InvalidPath(format!(
                    "duplicate PKG path `{}`",
                    item.name
                )));
            }
        }
        for path in paths.keys() {
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if paths.get(parent) == Some(&true) {
                    return Err(Error::InvalidPath(format!(
                        "PKG file `{parent}` is also a directory"
                    )));
                }
                ancestor = parent;
            }
        }
        Ok(())
    }

    fn read_item(archive: &mut PkgArchive<File>, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let Some(index) = archive
            .items()
            .iter()
            .position(|item| item.is_file && item.name == name)
        else {
            return Ok(None);
        };
        if archive.items()[index].file_size > 1024 * 1024 {
            return Err(Error::malformed(name, "metadata exceeds 1 MiB"));
        }
        archive
            .read_item_to_vec(index)
            .map(Some)
            .map_err(|error| Error::malformed(name, error.to_string()))
    }

    fn read_sfo(archive: &mut PkgArchive<File>) -> Result<Psf, Error> {
        let Some(bytes) = Self::read_item(archive, "PARAM.SFO")? else {
            return Ok(Psf::default());
        };
        let count = bytes
            .get(16..20)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
        if !count.is_some_and(|count| count as usize <= bytes.len().saturating_sub(20) / 16) {
            return Err(Error::malformed("PARAM.SFO", "invalid entry table"));
        }
        Psf::parse(&bytes).map_err(|error| Error::malformed("PARAM.SFO", error.to_string()))
    }

    /// Check the package footer digest, also detecting replacement after open().
    fn verified_file(&self, context: &InstallContext) -> Result<File, Error> {
        let mut file = File::open(&self.path).map_err(|error| Error::io(&self.path, error))?;
        let size = file
            .metadata()
            .map_err(|error| Error::io(&self.path, error))?
            .len();
        let body_size = size
            .checked_sub(32)
            .ok_or_else(|| Error::malformed("PKG", "missing package digest"))?;
        context.phase(crate::installable::InstallPhase::Verifying, body_size);
        let mut hash = Sha1::new();
        let mut reader = Reporting {
            inner: (&mut file).take(body_size),
            context: context.clone(),
        };
        let mut buffer = [0; 64 * 1024];
        loop {
            let len = reader
                .read(&mut buffer)
                .map_err(|error| Error::io(&self.path, error))?;
            if len == 0 {
                break;
            }
            hash.update(&buffer[..len]);
        }
        let mut digest = [0; 20];
        file.read_exact(&mut digest)
            .map_err(|error| Error::io(&self.path, error))?;
        if digest != self.digest || hash.finalize().as_slice() != digest {
            return Err(Error::IntegrityMatchFailed {
                failed: self.path.display().to_string(),
            });
        }
        Ok(file)
    }
}

impl Depot for Ps3PkgDepot {
    fn open(path: &Path) -> Result<Self, Error> {
        let mut file = File::open(path).map_err(|error| Error::io(path, error))?;
        let metadata = metadata::PackageMetadata::read(&mut file)?;
        let kind = metadata.depot_type()?;
        let size = file
            .metadata()
            .map_err(|error| Error::io(path, error))?
            .len();
        let mut digest = [0; 20];
        file.seek(SeekFrom::End(-32))
            .and_then(|_| file.read_exact(&mut digest))
            .map_err(|error| Error::io(path, error))?;
        let mut archive =
            PkgArchive::from_reader(file, size).map_err(|error| Error::io(path, error))?;
        if !archive.is_ps3() {
            return Err(Error::Unsupported(
                "a PS3 depot requires a PS3 PKG".to_owned(),
            ));
        }
        let title_id = archive.title_id().to_owned();
        let content_id = archive.content_id().to_owned();
        if TitleId::parse(&title_id).is_none() || !content_id.is_ascii() {
            return Err(Error::malformed("PKG", "invalid title or content ID"));
        }
        Self::validate_path(&content_id)?;
        if content_id.contains('/') {
            return Err(Error::InvalidPath(content_id));
        }
        Self::validate_entries(&archive)?;
        let sfo = Self::read_sfo(&mut archive)?;
        if sfo.get_string("TITLE_ID").is_some_and(|id| id != title_id) {
            return Err(Error::malformed(
                "PARAM.SFO",
                "TITLE_ID differs from the PKG content ID",
            ));
        }
        let label = crate::impls::sony::metadata::SonyMetadata::title(&sfo).map(str::to_owned);
        let icon = Self::read_item(&mut archive, "ICON0.PNG").ok().flatten();
        let directory = metadata.directory.as_deref().unwrap_or(&title_id);
        Self::validate_path(directory)?;
        if directory.contains('/') {
            return Err(Error::InvalidPath(directory.to_owned()));
        }
        let install_path = format!("dev_hdd0/game/{directory}");
        let app_version = sfo
            .get_string("APP_VER")
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned);
        let target_app_version = sfo
            .get_string("TARGET_APP_VER")
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned);
        let version = if kind == DepotType::AppContent {
            metadata.version
        } else {
            app_version.clone().or(metadata.version)
        };
        let contribution = PkgContribution {
            content_id: content_id.clone(),
            title_id: title_id.clone(),
            digest,
            app_version,
            target_app_version,
            files: archive
                .items()
                .iter()
                .filter(|item| item.is_file)
                .map(|item| {
                    (
                        format!("{install_path}/{}", item.name),
                        item.entry_type & 0x80000000 != 0,
                    )
                })
                .collect(),
        };
        Ok(Self {
            install_path,
            version,
            path: path.to_path_buf(),
            title_id,
            content_id,
            kind,
            digest,
            label,
            icon,
            contribution,
        })
    }

    fn preview(&self) -> Option<InstallPreview> {
        let icon = self
            .icon
            .as_deref()
            .and_then(|bytes| image::load_from_memory(bytes).ok())
            .and_then(|image| IconHint::image(image.into_rgba8()).ok())
            .and_then(|icon| match icon {
                IconHint::Embedded(icon) => Some(icon.data_url()),
                _ => None,
            });
        Some(InstallPreview {
            id: self.artifact_id(),
            label: self.label.clone().unwrap_or_else(|| self.title_id.clone()),
            depot_type: self.kind,
            icon,
        })
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if !(nand.platform() == Ps3Nand::PLATFORM && Ps3Nand::has_structure(&*nand.root())) {
            return Err(Error::IncompatibleNand);
        }
        if !nand.is_complete(crate::nand::NandCompletion::Minimal) {
            return Err(Error::Unsupported(
                "Install PlayStation 3 system software before using this NAND".into(),
            ));
        }
        Ok(())
    }

    fn depot_type(&self) -> DepotType {
        self.kind
    }

    async fn install<N: VirtualNand + ?Sized>(
        self,
        _nand: &N,
        _keys: &dyn crate::keys::KeyReader,
        context: &InstallContext,
    ) -> Result<impl Installable, Error> {
        let stage = crate::installable::ComponentStage::new()?;
        let component = self.contribution.component(&self.install_path, self.kind);
        let metadata = serde_json::to_value(&self.contribution)
            .map_err(|error| Error::malformed("PS3 component", error.to_string()))?;
        let inner = install::Ps3PkgInstall::new(&self, stage.storage.clone(), context).await?;
        Ok(stage.install(inner, component, metadata))
    }
}

impl Ps3PkgDepot {
    fn artifact_id(&self) -> String {
        let role = match self.kind {
            DepotType::App => "app",
            DepotType::AppUpdate => "update",
            DepotType::AppContent => "content",
            DepotType::System => "system",
        };
        format!("{}:{role}", self.content_id)
    }
}
