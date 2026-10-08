use super::depots::WiiUMetaXmlDepot;
use crate::Error;
use crate::nand::composition::{ComponentRevision, NandComposer};

pub(in crate::impls) struct WiiUComposer;

impl NandComposer for WiiUComposer {
    fn validate(&self, revision: &ComponentRevision) -> Result<(), Error> {
        let artifact = &revision.artifact;
        let id = u64::from_str_radix(artifact.id(), 16)
            .map_err(|_| Error::InvalidPath(artifact.id().into()))?;
        if id >> 48 != 5
            || WiiUMetaXmlDepot::component(id) != revision.component
            || WiiUMetaXmlDepot::title_kind(id) != artifact.depot_type()
            || !revision.metadata.is_null()
        {
            return Err(Error::HistoryConflict(
                "artifact does not match its component".into(),
            ));
        }
        Ok(())
    }
}
