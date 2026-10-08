mod package;
mod replay;

pub(in crate::impls::ps3) use package::PkgContribution;

use crate::installable::DepotType;
use crate::nand::composition::{ComponentRevision, Composition, NandComposer};
use crate::{ArtifactComponent, Error};
use std::collections::BTreeSet;

pub(in crate::impls) struct Ps3Composer;

impl Ps3Composer {
    pub(in crate::impls::ps3) fn firmware() -> ArtifactComponent {
        ArtifactComponent {
            id: "ps3:firmware".into(),
            title: "ps3:firmware".into(),
            root: "dev_flash".into(),
        }
    }
}

impl NandComposer for Ps3Composer {
    fn validate(&self, revision: &ComponentRevision) -> Result<(), Error> {
        if revision.component == Self::firmware() {
            if revision.artifact.id() != "ps3:firmware"
                || revision.artifact.depot_type() != DepotType::System
                || !revision.metadata.is_null()
            {
                return Err(Error::HistoryConflict(
                    "invalid PS3 firmware component".into(),
                ));
            }
            return Ok(());
        }
        PkgContribution::read(revision)?.validate(revision)
    }

    fn includes_ancestors(&self, component: &ArtifactComponent) -> bool {
        *component != Self::firmware()
    }

    fn check_install(
        &self,
        revision: &ComponentRevision,
        active: &[ComponentRevision],
    ) -> Result<(), Error> {
        let duplicate = if revision.component == Self::firmware() {
            active.iter().any(|old| {
                old.component.id == revision.component.id
                    && old.artifact.version() == revision.artifact.version()
            })
        } else {
            let incoming = PkgContribution::read(revision)?;
            let mut duplicate = false;
            for old in active
                .iter()
                .filter(|old| old.artifact.id() == revision.artifact.id())
            {
                duplicate |= PkgContribution::read(old)?.digest == incoming.digest
                    || (revision.artifact.version().is_some()
                        && old.artifact.version() == revision.artifact.version());
            }
            duplicate
        };
        if duplicate {
            return Err(Error::HistoryConflict(
                "this PS3 package revision is already installed; roll it back before reinstalling"
                    .into(),
            ));
        }
        Ok(())
    }

    fn compose(
        &self,
        revisions: &[ComponentRevision],
        _state: &serde_json::Value,
    ) -> Result<Composition, Error> {
        replay::Ps3Composition::compose(revisions)
    }

    fn retained_revision<'a>(
        &self,
        history: &'a [ComponentRevision],
        ids: &[&str],
    ) -> Option<&'a ComponentRevision> {
        let end = history
            .iter()
            .position(|revision| ids.contains(&revision.artifact.id()))
            .unwrap_or(history.len());
        history[..end].last()
    }

    fn affected(&self, revisions: &[ComponentRevision], ids: &[&str]) -> Vec<String> {
        let titles = revisions
            .iter()
            .filter(|revision| {
                ids.contains(&revision.artifact.id())
                    && revision.artifact.depot_type() == DepotType::App
            })
            .map(|revision| &revision.component.title)
            .collect::<BTreeSet<_>>();
        let firmware = ids.contains(&"ps3:firmware");
        revisions
            .iter()
            .filter(|revision| {
                !ids.contains(&revision.artifact.id())
                    && (firmware || titles.contains(&revision.component.title))
            })
            .map(|revision| revision.artifact.id().to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
