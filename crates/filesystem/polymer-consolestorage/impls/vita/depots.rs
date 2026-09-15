use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::impls::sony::{TitleId, metadata::SonyMetadata};
use crate::installable::artifacts::Artifact;
use crate::installable::{
    Depot, DepotFile, DepotType, DirectoryInstall, IconHint, Installable, Installation,
};
use crate::nand::VirtualNand;
use crate::{Error, VolumeStorage};

/// An already-decrypted Vita application, patch, or add-on directory.
/// PKG/PFS decryption and license installation are deliberately separate.
pub struct VitaDirectoryDepot {
    root: PathBuf,
    title_id: String,
    content_id: String,
    kind: DepotType,
    destination: String,
}

impl VitaDirectoryDepot {
    fn content_suffix<'a>(content_id: &'a str, title_id: &str) -> Result<&'a str, Error> {
        let valid = content_id.len() == 36
            && content_id.get(7..16) == Some(title_id)
            && content_id.as_bytes()[6] == b'-'
            && content_id.as_bytes()[16] == b'_'
            && content_id.as_bytes()[19] == b'-'
            && content_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !valid {
            return Err(Error::malformed(
                "sce_sys/param.sfo",
                "invalid DLC CONTENT_ID",
            ));
        }
        Ok(&content_id[20..])
    }

    pub fn title_id(&self) -> &str {
        &self.title_id
    }

    pub fn install_path(&self) -> &str {
        &self.destination
    }
}

impl Depot for VitaDirectoryDepot {
    fn open(root: &Path) -> Result<Self, Error> {
        let path = root.join("sce_sys/param.sfo");
        let sfo = SonyMetadata::read(
            File::open(&path).map_err(|error| Error::io(&path, error))?,
            &path.to_string_lossy(),
        )?;
        let title_id = sfo.get_string("TITLE_ID").unwrap_or_default();
        if TitleId::parse(title_id).is_none() {
            return Err(Error::malformed(
                path.to_string_lossy(),
                "invalid Vita TITLE_ID",
            ));
        }
        let content_id = sfo.get_string("CONTENT_ID").unwrap_or_default();
        let (kind, destination) = match sfo.get_string("CATEGORY") {
            Some("gd") => (DepotType::App, format!("ux0/app/{title_id}")),
            Some("gp") => (DepotType::AppUpdate, format!("ux0/patch/{title_id}")),
            Some("ac") => {
                let suffix = Self::content_suffix(content_id, title_id)?;
                (
                    DepotType::AppContent,
                    format!("ux0/addcont/{title_id}/{suffix}"),
                )
            }
            category => {
                return Err(Error::Unsupported(format!(
                    "Vita depot category {category:?}"
                )));
            }
        };
        let depot = Self {
            root: root.to_owned(),
            title_id: title_id.to_owned(),
            content_id: content_id.to_owned(),
            kind,
            destination,
        };
        let missing = depot.validate_contents();
        if !missing.is_empty() {
            return Err(Error::InvalidDepot {
                root: root.display().to_string(),
                missing,
            });
        }
        Ok(depot)
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if nand.is_dirty()? {
            return Err(Error::HistoryConflict(
                "This NAND has uncommitted changes. Recover it in Console storage before installing.".into(),
            ));
        }
        if !(nand.platform() == super::VitaNand::PLATFORM) {
            return Err(Error::Unsupported(
                "Depot requires a compatible NAND with its storage layout initialized".into(),
            ));
        }
        Ok(())
    }
    fn validate_contents(&self) -> Vec<String> {
        let mut invalid = Vec::new();
        if !self.root.join("sce_sys/param.sfo").is_file() {
            invalid.push("sce_sys/param.sfo".to_owned());
        }
        if matches!(self.kind, DepotType::App | DepotType::AppUpdate)
            && !self.root.join("eboot.bin").is_file()
        {
            invalid.push("eboot.bin".to_owned());
        }
        if self.root.join("sce_pfs").exists() {
            invalid.push("sce_pfs (encrypted content requires PFS decryption)".to_owned());
        }
        // DirectoryInstall must never follow an input link outside the depot.
        for entry in walkdir::WalkDir::new(&self.root) {
            match entry {
                Ok(entry) if entry.file_type().is_file() || entry.file_type().is_dir() => {}
                Ok(entry) => invalid.push(entry.path().display().to_string()),
                Err(error) => invalid.push(error.to_string()),
            }
        }
        invalid
    }

    fn depot_type(&self) -> DepotType {
        self.kind
    }

    fn install<'a, N: VirtualNand + ?Sized>(self, nand: &'a N) -> Installation<'a>
    where
        Self: 'a,
    {
        let source = self;

        Installation::prepare(async move {
            source.satisfies(nand)?;
            let storage = nand.root();
            Ok(VitaInstall {
                copy: DirectoryInstall::new(
                    source.root.clone(),
                    source.destination.clone(),
                    storage.clone(),
                ),
                depot: source,
                storage,
            })
        })
    }
}

struct VitaInstall {
    depot: VitaDirectoryDepot,
    copy: DirectoryInstall,
    storage: Arc<dyn VolumeStorage>,
}

impl Installable for VitaInstall {
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error> {
        self.copy.next_batch().await
    }

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let root = &self.depot.destination;
        let path = format!("{root}/sce_sys/param.sfo");
        let sfo = SonyMetadata::read(self.storage.open_read(&path)?, &path)?;
        if sfo.get_string("TITLE_ID") != Some(self.depot.title_id.as_str())
            || sfo.get_string("CONTENT_ID").unwrap_or_default() != self.depot.content_id
        {
            return Err(Error::malformed(path, "installed Vita identity changed"));
        }
        let category = match self.depot.kind {
            DepotType::App => "gd",
            DepotType::AppUpdate => "gp",
            DepotType::AppContent => "ac",
            _ => unreachable!("Vita directory depot category"),
        };
        if sfo.get_string("CATEGORY") != Some(category) {
            return Err(Error::malformed(path, "installed Vita category changed"));
        }
        let icon = IconHint::read(
            &*self.storage,
            &format!("{root}/sce_sys/icon0.png"),
            self.depot.kind,
        )?;
        let artifact = Artifact::new(
            format!("{}:{}", root, self.depot.content_id),
            self.depot.kind,
            manifest,
            crate::impls::sony::metadata::SonyMetadata::title(&sfo)
                .unwrap_or(&self.depot.title_id)
                .to_owned(),
            sfo.get_string("APP_VER")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned),
            icon,
        );
        self.storage.artifacts().record(&artifact).await?;
        Ok(artifact)
    }
}
