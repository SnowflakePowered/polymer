use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::impls::wiiu::{nand::WiiUNand, xml};
use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};
use crate::installable::{
    Depot, DepotFile, DepotType, DirectoryInstall, IconHint, InPlaceDepot, InstallContext,
    Installable,
};
use crate::nand::VirtualNand;
use crate::requires_structure;
use crate::{ArtifactComponent, Storage};

/// A Wii U title dumped as a loose `code`/`content`/`meta` tree.
pub struct WiiUMetaXmlDepot {
    root: PathBuf,
    /// The depot own title id, from `code/app.xml`.
    title_id: u64,
    /// The title id `meta/meta.xml` presents as. For an update this is the title
    /// being updated, not the update itself.
    meta_title_id: u64,
}

impl WiiUMetaXmlDepot {
    pub fn title_id(&self) -> u64 {
        self.title_id
    }

    /// The title this depot presents itself as, which differs from
    /// [`Self::title_id`] only for updates.
    pub fn meta_title_id(&self) -> u64 {
        self.meta_title_id
    }

    /// Where this title lives in a Wii U NAND.
    pub fn install_path(&self) -> String {
        Self::component(self.title_id).root
    }

    pub(crate) fn component(id: u64) -> ArtifactComponent {
        let title = match (id >> 32) as u32 {
            0x0005000c | 0x0005000e => 0x0005000000000000 | (id & 0xffffffff),
            _ => id,
        };
        ArtifactComponent {
            id: format!("usr/title/{:08x}/{:08x}", (id >> 32) as u32, id as u32),
            title: format!("{title:016x}"),
            root: format!("usr/title/{:08x}/{:08x}", (id >> 32) as u32, id as u32),
        }
    }

    pub(crate) fn title_kind(id: u64) -> DepotType {
        match (id >> 32) as u32 {
            0x0005000C => DepotType::AppContent,
            0x0005000E => DepotType::AppUpdate,
            0x00050000 | 0x00050002 => DepotType::App,
            _ => DepotType::System,
        }
    }

    fn artifact(self, storage: &dyn Storage, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let root = self.install_path();
        let path = format!("{root}/code/app.xml");
        let app: xml::AppXml =
            xml::read_from(std::io::BufReader::new(storage.open_read(&path)?), &path)?;
        if app.title_id.0 != self.title_id {
            return Err(Error::malformed(path, "installed Wii U identity changed"));
        }
        let path = format!("{root}/meta/meta.xml");
        let meta: xml::MetaXml =
            xml::read_from(std::io::BufReader::new(storage.open_read(&path)?), &path)?;
        let kind = self.depot_type();
        let icon = IconHint::read(storage, &format!("{root}/meta/iconTex.tga"), kind)?;
        let label = meta
            .name()
            .unwrap_or_else(|| format!("{:016x}", app.title_id.0));
        let version = Some(meta.title_version.text.trim().to_owned()).filter(|s| !s.is_empty());
        let entrypoint = match kind {
            DepotType::App => {
                let path = format!("{root}/code/cos.xml");
                let cos: xml::CosXml =
                    xml::read_from(std::io::BufReader::new(storage.open_read(&path)?), &path)?;
                Some(ArtifactEntrypoint {
                    root: root.clone(),
                    path: format!("code/{}", cos.argstr.text.trim()),
                    package: root.clone(),
                })
            }
            _ => None,
        };
        Artifact::new(
            format!("{:016x}", app.title_id.0),
            kind,
            manifest,
            label,
            version,
            vec![icon],
            entrypoint,
        )
    }
}

impl InPlaceDepot for WiiUMetaXmlDepot {
    const PLATFORM: &'static str = WiiUNand::PLATFORM;
    const MARKERS: &'static [&'static str] = &["meta/meta.xml", "code/app.xml", "code/cos.xml"];

    fn locate(path: &Path) -> Result<Self, Error> {
        <Self as Depot>::open(path)
    }

    fn title_at(install_path: &str) -> Option<String> {
        let (high, low) = install_path.strip_prefix("usr/title/")?.split_once('/')?;
        let id = (u64::from_str_radix(high, 16).ok()? << 32) | u64::from_str_radix(low, 16).ok()?;
        Some(Self::component(id).title)
    }

    fn title(&self) -> String {
        Self::component(self.title_id).title
    }

    fn kind(&self) -> DepotType {
        Self::title_kind(self.title_id)
    }

    fn install_path(&self) -> String {
        Self::component(self.title_id).root
    }

    fn entrypoint(&self) -> Result<Option<ArtifactEntrypoint>, Error> {
        if self.kind() != DepotType::App {
            return Ok(None);
        }
        let cos: xml::CosXml = xml::read(&self.root.join("code/cos.xml"))?;
        let root = InPlaceDepot::install_path(self);
        Ok(Some(ArtifactEntrypoint {
            root: root.clone(),
            path: format!("code/{}", cos.argstr.text.trim()),
            package: root,
        }))
    }

    fn keys(&self) -> Result<Vec<crate::keys::KeyRecord>, Error> {
        self.embedded_keys()
    }
}

impl Depot for WiiUMetaXmlDepot {
    fn embedded_keys(&self) -> Result<Vec<crate::keys::KeyRecord>, Error> {
        let ticket = self.root.join("code/title.tik");
        if !ticket
            .try_exists()
            .map_err(|error| Error::io(&ticket, error))?
        {
            return Ok(Vec::new());
        }
        let bytes = std::fs::read(&ticket).map_err(|error| Error::io(&ticket, error))?;
        let record = super::ticket::WiiUTicket::decode(&bytes)?;
        if record.identity.id != format!("{:016x}", self.title_id) {
            return Err(Error::malformed(
                "Wii U ticket",
                "ticket and app.xml title IDs differ",
            ));
        }
        Ok(vec![record])
    }

    fn open(path: &Path) -> Result<WiiUMetaXmlDepot, Error> {
        requires_structure!(
            path,
            "meta/",
            "meta/meta.xml",
            "code/",
            "code/app.xml",
            "code/cos.xml",
            "content/"
        )?;

        let app: xml::AppXml = xml::read(&path.join("code/app.xml"))?;
        let meta: xml::MetaXml = xml::read(&path.join("meta/meta.xml"))?;

        Ok(Self {
            root: path.to_path_buf(),
            title_id: app.title_id.0,
            meta_title_id: meta.title_id.0,
        })
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if nand.platform() != WiiUNand::PLATFORM {
            return Err(Error::IncompatibleNand);
        }
        let storage = nand.root();
        requires_structure!(
            &*storage as &dyn Storage,
            "sys/",
            "usr/",
            "usr/title/",
            "usr/title/0005000e/", // game updates
            "usr/title/00050000/", // games
            "usr/title/0005000c/", // DLC
        )
        .map_err(|_| Error::IncompatibleNand)
    }

    fn depot_type(&self) -> DepotType {
        Self::title_kind(self.title_id)
    }

    async fn install<N: VirtualNand + ?Sized>(
        self,
        _nand: &N,
        _keys: &dyn crate::keys::KeyReader,
        context: &InstallContext,
    ) -> Result<impl Installable, Error> {
        let stage = crate::installable::ComponentStage::new()?;
        let storage = stage.storage.clone();
        let component = Self::component(self.title_id);
        let inner = DirectoryInstall::new(
            self.root.clone(),
            self.install_path(),
            storage.clone(),
            context,
            move |manifest| self.artifact(&*storage, manifest),
        )
        .excluding("code/title.tik");
        Ok(stage.install(inner, component, serde_json::Value::Null))
    }
}
