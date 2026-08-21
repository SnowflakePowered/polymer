use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::Stream;
use futures::channel::oneshot;
use futures::future::TryFutureExt;

use crate::Storage;
use crate::error::Error;
use crate::impls::wiiu::xml;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::installable::{Depot, DepotType, DirectoryInstall, IconHint};
use crate::nand::VirtualNand;
use crate::requires_structure;

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
    label: String,
    version: Option<String>,
}

impl WiiUMetaXmlDepot {
    /// Try to load the folder as a depot
    pub fn new(path: &Path) -> Result<WiiUMetaXmlDepot, Error> {
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

        let label = meta.longname_en.0.clone();
        let version = Some(meta.title_version.text.trim().to_string()).filter(|v| !v.is_empty());

        Ok(Self {
            root: path.to_path_buf(),
            title_id: app.title_id.0,
            meta_title_id: meta.title_id.0,
            label,
            version,
        })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn meta_xml(&self) -> PathBuf {
        self.root.join("meta/meta.xml")
    }

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
    fn satisfies<N: VirtualNand>(&self, nand: &N) -> bool {
        if nand.platform() != "NINTENDO_WIIU" {
            return false;
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
        .is_ok()
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

    fn install<'a, N: VirtualNand>(
        &'a self,
        nand: &'a N,
    ) -> (
        Box<dyn Future<Output = Result<Artifact, Error>>>,
        Box<dyn Stream<Item = Result<String, Error>> + 'a>,
    ) {
        let (sender, receiver) = oneshot::channel();

        let storage: Arc<dyn Storage> = nand.root();
        let install = DirectoryInstall {
            root: self.root.clone(),
            base: self.install_path(),
            storage,
            artifact: self,
        };

        let artifact = receiver.unwrap_or_else(|_| Err(Error::InstallIncomplete));

        (Box::new(artifact), Box::new(install.run(sender)))
    }
}

impl DescribeArtifact for WiiUMetaXmlDepot {
    fn describe(&self) -> Artifact {
        let icon = match self.root.join("meta/iconTex.tga").is_file() {
            true => IconHint::File(PathBuf::from("meta/iconTex.tga")),
            false => IconHint::Builtin(self.depot_type()),
        };

        Artifact::new(
            format!("{:016x}", self.title_id),
            self.depot_type(),
            Vec::new(),
            self.label.clone(),
            self.version.clone(),
            icon,
        )
    }
}
