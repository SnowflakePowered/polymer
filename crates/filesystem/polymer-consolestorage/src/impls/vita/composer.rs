use crate::Error;
use crate::installable::DepotType;
use crate::nand::composition::{ComponentRevision, NandComposer};

pub(in crate::impls) struct VitaComposer;

impl NandComposer for VitaComposer {
    fn validate(&self, revision: &ComponentRevision) -> Result<(), Error> {
        let component = &revision.component;
        let artifact = &revision.artifact;
        if crate::impls::sony::TitleId::parse(&component.title).is_none() {
            return Err(Error::InvalidPath(component.title.clone()));
        }
        let expected = match artifact.depot_type() {
            DepotType::App => format!("ux0/app/{}", component.title),
            DepotType::AppUpdate => format!("ux0/patch/{}", component.title),
            DepotType::AppContent => {
                let prefix = format!("ux0/addcont/{}/", component.title);
                let suffix = component
                    .root
                    .strip_prefix(&prefix)
                    .filter(|suffix| !suffix.is_empty() && !suffix.contains('/'))
                    .ok_or_else(|| Error::InvalidPath(component.root.clone()))?;
                format!("{prefix}{suffix}")
            }
            _ => return Err(Error::Unsupported("Vita system component".into())),
        };
        if component.root != expected
            || component.id != expected
            || !artifact.id().starts_with(&format!("{}:", component.root))
            || !revision.metadata.is_null()
        {
            return Err(Error::HistoryConflict(
                "artifact does not match its component".into(),
            ));
        }
        Ok(())
    }
}
