mod install;
mod reader;
mod sce;
#[cfg(test)]
mod tests;

use super::super::Ps3Nand;
use crate::Error;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::installable::{Depot, DepotType, IconHint};
use crate::nand::VirtualNand;
use futures::channel::oneshot;
use futures::future::{Either, TryFutureExt};
use futures::{StreamExt, stream};
use std::path::{Path, PathBuf};

/// PS3 system firmware distributed as a PS3UPDAT.PUP archive.
#[derive(Clone)]
pub struct Ps3PupDepot {
    path: PathBuf,
    version: String,
}
impl Ps3PupDepot {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut pup = reader::Pup::open(path).map_err(|e| Error::io(path, e))?;
        let version = pup.version().map_err(|e| Error::io(path, e))?;
        pup.entry(0x300).map_err(|e| Error::io(path, e))?;
        Ok(Self {
            path: path.to_owned(),
            version,
        })
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    fn verified_pup(&self) -> Result<reader::Pup, Error> {
        let mut pup = reader::Pup::open(&self.path).map_err(|e| Error::io(&self.path, e))?;
        pup.verify()
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
impl DescribeArtifact for Ps3PupDepot {
    fn describe(&self) -> Artifact {
        Artifact::new(
            "ps3:firmware".to_owned(),
            DepotType::System,
            Vec::new(),
            "PlayStation 3 system software".to_owned(),
            Some(self.version.clone()),
            IconHint::Builtin(DepotType::System),
        )
    }
}
impl Depot for Ps3PupDepot {
    fn satisfies<N: VirtualNand>(&self, nand: &N) -> bool {
        nand.platform() == Ps3Nand::PLATFORM && Ps3Nand::has_structure(&*nand.root())
    }
    fn validate_contents(&self) -> Vec<String> {
        match self.verified_pup() {
            Ok(_) => Vec::new(),
            Err(_) => vec![self.path.display().to_string()],
        }
    }
    fn depot_type(&self) -> DepotType {
        DepotType::System
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
                install::PupInstall::new(self, nand.root()).await
            } else {
                Err(Error::Unsupported(format!(
                    "a PS3 PUP cannot be installed to {}",
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
