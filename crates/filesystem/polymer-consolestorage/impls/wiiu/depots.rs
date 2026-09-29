use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Error;
use crate::impls::wiiu::xml;
use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};
use crate::installable::{Depot, DepotFile, DepotType, DirectoryInstall, IconHint, Installable};
use crate::nand::VirtualNand;
use crate::requires_structure;
use crate::{Storage, VolumeStorage};

pub struct DumplingDepot {}

// todo: this is an OS depot.
impl DumplingDepot {
    pub fn new(path: &Path) -> Result<DumplingDepot, Error> {
        requires_structure!(path, "otp.bin", "seeprom.bin",)?;

        Ok(Self {
      // root: path.to_path_buf(),
    })
    }
}

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
        format!(
            "usr/title/{:08x}/{:08x}",
            (self.title_id >> 32) as u32,
            self.title_id as u32
        )
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
        if nand.platform() != "NINTENDO_WIIU" {
            return Err(Error::Unsupported(
                "Depot belongs to a different platform".into(),
            ));
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
        )?;
        Ok(())
    }
    fn validate_contents(&self) -> Vec<String> {
        Vec::new()
    }

    fn depot_type(&self) -> DepotType {
        match (self.title_id >> 32) as u32 {
            0x0005000C => DepotType::AppContent,
            0x0005000E => DepotType::AppUpdate,
            0x00050000 | 0x00050002 => DepotType::App,
            _ => DepotType::System,
        }
    }

    fn install<'a, N: VirtualNand + ?Sized>(
        self,
        nand: &'a N,
        events: crate::installable::InstallEvents,
    ) -> crate::installable::Installation<'a>
    where
        Self: 'a,
    {
        let source = self;

        crate::installable::Installation::prepare(async move {
            source.validate(nand, &source.root)?;
            let storage = nand.root();
            Ok(WiiUInstall {
                events: events.clone(),
                copy: DirectoryInstall::new(
                    source.root.clone(),
                    source.install_path(),
                    storage.clone(),
                    events.clone(),
                )
                .excluding("code/title.tik"),
                depot: source,
                storage,
            })
        })
    }
}

struct WiiUInstall {
    depot: WiiUMetaXmlDepot,
    copy: DirectoryInstall,
    storage: Arc<dyn VolumeStorage>,
    events: crate::installable::InstallEvents,
}

impl Installable for WiiUInstall {
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error> {
        self.copy.next_batch().await
    }

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let root = self.depot.install_path();
        let path = format!("{root}/code/app.xml");
        let app: xml::AppXml = xml::read_from(
            std::io::BufReader::new(self.storage.open_read(&path)?),
            &path,
        )?;
        if app.title_id.0 != self.depot.title_id {
            return Err(Error::malformed(path, "installed Wii U identity changed"));
        }
        let path = format!("{root}/meta/meta.xml");
        let meta: xml::MetaXml = xml::read_from(
            std::io::BufReader::new(self.storage.open_read(&path)?),
            &path,
        )?;
        let kind = self.depot.depot_type();
        let icon = IconHint::read(&*self.storage, &format!("{root}/meta/iconTex.tga"), kind)?;
        let label = meta
            .name()
            .unwrap_or_else(|| format!("{:016x}", app.title_id.0));
        let version = Some(meta.title_version.text.trim().to_owned()).filter(|s| !s.is_empty());
        let entrypoint = match kind {
            DepotType::App => {
                let path = format!("{root}/code/cos.xml");
                let cos: xml::CosXml = xml::read_from(
                    std::io::BufReader::new(self.storage.open_read(&path)?),
                    &path,
                )?;
                Some(ArtifactEntrypoint {
                    root: root.clone(),
                    path: format!("code/{}", cos.argstr.text.trim()),
                })
            }
            _ => None,
        };
        let artifact = Artifact::new(
            format!("{:016x}", app.title_id.0),
            kind,
            manifest,
            label,
            version,
            vec![icon],
            entrypoint,
        )?;
        self.storage
            .artifacts()
            .record(&artifact, self.events)
            .await?;
        Ok(artifact)
    }
}
