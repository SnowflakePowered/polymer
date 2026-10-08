use super::CiaDepot;
use crate::Error;
use crate::installable::DepotType;
use crate::nand::composition::{ComponentRevision, NandComposer};

pub(in crate::impls) struct CtrComposer;

impl NandComposer for CtrComposer {
    fn validate(&self, revision: &ComponentRevision) -> Result<(), Error> {
        let artifact = &revision.artifact;
        let id = u64::from_str_radix(artifact.id(), 16)
            .map_err(|_| Error::InvalidPath(artifact.id().into()))?;
        if id >> 48 != 4
            || CiaDepot::component(id) != revision.component
            || CiaDepot::title_kind(id) != artifact.depot_type()
            || !revision.metadata.is_null()
        {
            return Err(Error::HistoryConflict(
                "artifact does not match its component".into(),
            ));
        }
        Ok(())
    }

    fn check_install(
        &self,
        revision: &ComponentRevision,
        active: &[ComponentRevision],
    ) -> Result<(), Error> {
        let Some(previous) = active
            .iter()
            .find(|old| old.component.id == revision.component.id)
        else {
            return Ok(());
        };
        let artifact = &revision.artifact;
        let previous = &previous.artifact;
        if previous.version() != artifact.version() {
            return Ok(());
        }
        let extension = artifact.depot_type() == DepotType::AppContent
            && artifact.manifest().len() > previous.manifest().len()
            && previous.manifest().iter().all(|old| {
                artifact.manifest().iter().any(|new| {
                    old.path() == new.path()
                        && old.blake3() == new.blake3()
                        && old.byte_length() == new.byte_length()
                })
            });
        if !extension {
            return Err(Error::HistoryConflict(
                "this component version is already installed; uninstall it before reinstalling"
                    .into(),
            ));
        }
        Ok(())
    }
}
