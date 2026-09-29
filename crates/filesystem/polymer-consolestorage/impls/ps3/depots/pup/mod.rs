mod install;
mod reader;
mod sce;

use super::super::Ps3Nand;
use crate::Error;
use crate::installable::{Depot, DepotType, Installable};
use crate::nand::VirtualNand;
use std::path::{Path, PathBuf};

/// PS3 system firmware distributed as a PS3UPDAT.PUP archive.
#[derive(Clone)]
pub struct Ps3PupDepot {
    path: PathBuf,
    version: String,
}
impl Ps3PupDepot {
    pub fn version(&self) -> &str {
        &self.version
    }
    fn verified_pup(
        &self,
        events: &crate::installable::InstallEvents,
    ) -> Result<reader::Pup, Error> {
        let mut pup = reader::Pup::open(&self.path).map_err(|e| Error::io(&self.path, e))?;
        pup.verify(events)
            .map_err(|e| Error::malformed("PUP", e.to_string()))?;
        if pup.version().map_err(|e| Error::io(&self.path, e))? != self.version {
            return Err(Error::malformed(
                "PUP",
                "firmware version changed after opening",
            ));
        }
        Ok(pup)
    }
}

impl Depot for Ps3PupDepot {
    fn open(path: &Path) -> Result<Self, Error> {
        let mut pup = reader::Pup::open(path).map_err(|e| Error::io(path, e))?;
        let version = pup.version().map_err(|e| Error::io(path, e))?;
        pup.entry(0x300).map_err(|e| Error::io(path, e))?;
        Ok(Self {
            path: path.to_owned(),
            version,
        })
    }

    fn satisfies<N: VirtualNand + ?Sized>(&self, nand: &N) -> Result<(), Error> {
        if !(nand.platform() == Ps3Nand::PLATFORM && Ps3Nand::has_structure(&*nand.root())) {
            return Err(Error::IncompatibleNand);
        }
        Ok(())
    }
    fn depot_type(&self) -> DepotType {
        DepotType::System
    }
    async fn install<N: VirtualNand + ?Sized>(
        self,
        nand: &N,
        _keys: &dyn crate::keys::KeyReader,
        events: crate::installable::InstallEvents,
    ) -> Result<impl Installable, Error> {
        install::PupInstall::new(&self, nand.root(), events).await
    }
}
