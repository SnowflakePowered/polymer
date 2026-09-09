mod install;
mod metadata;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use futures::channel::oneshot;
use futures::future::{Either, TryFutureExt};
use futures::{StreamExt, stream};
use orbis_unpkg::Psf;
use pkgrip::PkgArchive;
use sha1::{Digest, Sha1};

use super::super::{Ps3Nand, TitleId};
use crate::Error;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::installable::{Depot, DepotType, IconHint};
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
    label: String,
    version: Option<String>,
    icon: bool,
    digest: [u8; 20],
}

impl Ps3PkgDepot {
    pub fn open(path: &Path) -> Result<Self, Error> {
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
        let directory = metadata.directory.as_deref().unwrap_or(&title_id);
        Self::validate_path(directory)?;
        if directory.contains('/') {
            return Err(Error::InvalidPath(directory.to_owned()));
        }
        Ok(Self {
            install_path: format!("dev_hdd0/game/{directory}"),
            label: sfo
                .get_string("TITLE")
                .filter(|title| !title.is_empty())
                .unwrap_or(&title_id)
                .to_owned(),
            version: sfo
                .get_string("APP_VER")
                .filter(|version| !version.is_empty())
                .map(str::to_owned)
                .or(metadata.version),
            icon: archive
                .items()
                .iter()
                .any(|item| item.is_file && item.name == "ICON0.PNG"),
            path: path.to_path_buf(),
            title_id,
            content_id,
            kind,
            digest,
        })
    }

    pub fn title_id(&self) -> &str {
        &self.title_id
    }
    pub fn content_id(&self) -> &str {
        &self.content_id
    }
    pub fn install_path(&self) -> &str {
        &self.install_path
    }

    fn validate_path(path: &str) -> Result<(), Error> {
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

    fn read_sfo(archive: &mut PkgArchive<File>) -> Result<Psf, Error> {
        let Some(index) = archive
            .items()
            .iter()
            .position(|item| item.is_file && item.name == "PARAM.SFO")
        else {
            return Ok(Psf::default());
        };
        if archive.items()[index].file_size > 1024 * 1024 {
            return Err(Error::malformed("PARAM.SFO", "metadata exceeds 1 MiB"));
        }
        let bytes = archive
            .read_item_to_vec(index)
            .map_err(|error| Error::malformed("PARAM.SFO", error.to_string()))?;
        let count = bytes
            .get(16..20)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
        if !count.is_some_and(|count| count as usize <= bytes.len().saturating_sub(20) / 16) {
            return Err(Error::malformed("PARAM.SFO", "invalid entry table"));
        }
        Psf::parse(&bytes).map_err(|error| Error::malformed("PARAM.SFO", error.to_string()))
    }

    /// Check the package footer digest, also detecting replacement after open().
    fn verified_file(&self) -> Result<File, Error> {
        let mut file = File::open(&self.path).map_err(|error| Error::io(&self.path, error))?;
        let size = file
            .metadata()
            .map_err(|error| Error::io(&self.path, error))?
            .len();
        let body_size = size
            .checked_sub(32)
            .ok_or_else(|| Error::malformed("PKG", "missing package digest"))?;
        let mut hash = Sha1::new();
        let mut reader = (&mut file).take(body_size);
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

impl DescribeArtifact for Ps3PkgDepot {
    fn describe(&self) -> Artifact {
        // Base games and patches may share a content ID and install directory.
        let role = match self.kind {
            DepotType::App => "app",
            DepotType::AppUpdate => "update",
            DepotType::AppContent => "content",
            DepotType::System => "system",
        };
        Artifact::new(
            format!("{}:{role}", self.content_id),
            self.kind,
            Vec::new(),
            self.label.clone(),
            self.version.clone(),
            if self.icon {
                IconHint::File(PathBuf::from(format!("{}/ICON0.PNG", self.install_path)))
            } else {
                IconHint::Builtin(self.kind)
            },
        )
    }
}

impl Depot for Ps3PkgDepot {
    fn satisfies<N: VirtualNand>(&self, nand: &N) -> bool {
        nand.platform() == Ps3Nand::PLATFORM && Ps3Nand::has_structure(&*nand.root())
    }

    fn validate_contents(&self) -> Vec<String> {
        match self.verified_file() {
            Ok(_) => Vec::new(),
            Err(_) => vec![self.path.display().to_string()],
        }
    }

    fn depot_type(&self) -> DepotType {
        self.kind
    }

    fn install<'a, N: VirtualNand>(
        &'a self,
        nand: &'a N,
    ) -> (
        Box<dyn Future<Output = Result<Artifact, Error>>>,
        Box<dyn futures::Stream<Item = Result<String, Error>> + 'a>,
    ) {
        let (sender, receiver) = oneshot::channel();
        let operation = stream::once(async move {
            let state = if self.satisfies(nand) {
                install::Install::new(self, nand.root()).await
            } else {
                Err(Error::Unsupported(format!(
                    "a PS3 PKG cannot be installed to {}",
                    nand.platform()
                )))
            };
            match state {
                Ok(state) => Either::Left(state.run(sender)),
                Err(error) => {
                    let _ = sender.send(Err(error.clone()));
                    Either::Right(stream::once(async move { Err(error) }))
                }
            }
        })
        .flatten();
        (
            Box::new(receiver.unwrap_or_else(|_| Err(Error::InstallIncomplete))),
            Box::new(operation),
        )
    }
}
