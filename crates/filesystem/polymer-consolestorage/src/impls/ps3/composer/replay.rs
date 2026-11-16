use super::{PkgContribution, Ps3Composer};
use crate::Error;
use crate::nand::composition::{ComponentRevision, Composition, FileContribution};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Ps3Composition {
    output: Composition,
    versions: BTreeMap<String, Option<String>>,
    spellings: BTreeMap<String, String>,
    unsatisfied: BTreeSet<String>,
}

impl Ps3Composition {
    pub fn compose(revisions: &[ComponentRevision]) -> Result<Composition, Error> {
        let mut replay = Self::default();
        let firmware = revisions
            .iter()
            .find(|revision| revision.component == Ps3Composer::firmware());
        if let Some(firmware) = firmware {
            replay.output.insert(firmware);
        }
        let mut packages = revisions
            .iter()
            .filter(|revision| revision.component != Ps3Composer::firmware())
            .collect::<Vec<_>>();
        packages.sort_by_key(|revision| revision.sequence);
        for revision in packages {
            if firmware.is_none() {
                replay.unsatisfied.insert(revision.artifact.id().to_owned());
            }
            replay.apply(revision)?;
        }
        if !replay.unsatisfied.is_empty() {
            return Err(Error::AffectedDepots {
                affected: replay.unsatisfied.into_iter().collect(),
            });
        }
        replay.output.validate()?;
        Ok(replay.output)
    }

    fn apply(&mut self, revision: &ComponentRevision) -> Result<(), Error> {
        let package = PkgContribution::read(revision)?;
        let root = &revision.component.root;
        let current = self
            .versions
            .get(root)
            .and_then(|version| version.as_deref());
        if !package.accepts_version(revision.artifact.depot_type(), current)? {
            self.unsatisfied.insert(revision.artifact.id().to_owned());
        }
        for (path, overwrite) in &package.files {
            if let Some(existing) = self.spellings.insert(path.to_lowercase(), path.clone())
                && existing != *path
            {
                return Err(Error::HistoryConflict(format!(
                    "conflicting PS3 file spelling: {path}"
                )));
            }
            if !overwrite && self.output.files.contains_key(path) {
                continue;
            }
            self.output.files.insert(
                path.clone(),
                FileContribution {
                    revision: revision.revision.clone(),
                    path: path.clone(),
                },
            );
            if path == &format!("{root}/PARAM.SFO") {
                self.versions
                    .insert(root.clone(), package.app_version.clone());
            }
        }
        self.output
            .directories
            .extend(revision.directories.iter().cloned());
        self.output
            .artifacts
            .insert(revision.artifact.id().to_owned(), revision.artifact.clone());
        Ok(())
    }
}
