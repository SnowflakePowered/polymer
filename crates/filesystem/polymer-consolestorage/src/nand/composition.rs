use std::collections::{BTreeMap, BTreeSet};

use crate::installable::{DepotType, artifacts::Artifact};
use crate::{ArtifactComponent, Error};

#[derive(Clone)]
pub struct ComponentRevision {
    pub component: ArtifactComponent,
    pub artifact: Artifact,
    pub revision: String,
    pub sequence: u64,
    pub metadata: serde_json::Value,
    pub directories: BTreeSet<String>,
}

pub struct FileContribution {
    pub revision: String,
    pub path: String,
}

#[derive(Default)]
pub struct Composition {
    pub files: BTreeMap<String, FileContribution>,
    pub generated: BTreeMap<String, Vec<u8>>,
    pub directories: BTreeSet<String>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub state: serde_json::Value,
}

impl Composition {
    pub fn disjoint(revisions: &[ComponentRevision]) -> Result<Self, Error> {
        let mut composition = Self::default();
        let mut roots = BTreeSet::new();
        for revision in revisions {
            let root = &revision.component.root;
            if roots.iter().any(|old: &&String| {
                *old == root
                    || old.starts_with(&format!("{root}/"))
                    || root.starts_with(&format!("{old}/"))
            }) {
                return Err(Error::HistoryConflict("overlapping components".into()));
            }
            roots.insert(root);
            composition.insert(revision);
        }
        composition.validate()?;
        Ok(composition)
    }

    pub fn insert(&mut self, revision: &ComponentRevision) {
        for file in revision.artifact.manifest() {
            let path = file.path().to_string_lossy().replace('\\', "/");
            self.files.insert(
                path.clone(),
                FileContribution {
                    revision: revision.revision.clone(),
                    path,
                },
            );
        }
        self.directories
            .extend(revision.directories.iter().cloned());
        self.artifacts
            .insert(revision.artifact.id().to_owned(), revision.artifact.clone());
    }

    pub fn validate(&self) -> Result<(), Error> {
        let mut files = BTreeSet::new();
        for path in self.files.keys().chain(self.generated.keys()) {
            if !files.insert(path.to_lowercase()) {
                return Err(Error::HistoryConflict(format!(
                    "conflicting file spelling: {path}"
                )));
            }
        }
        for path in self
            .files
            .keys()
            .chain(self.generated.keys())
            .chain(&self.directories)
        {
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if files.contains(&parent.to_lowercase()) {
                    return Err(Error::HistoryConflict(format!(
                        "file is also a directory: {parent}"
                    )));
                }
                ancestor = parent;
            }
        }
        for path in &self.directories {
            if files.contains(&path.to_lowercase()) {
                return Err(Error::HistoryConflict(format!(
                    "file is also a directory: {path}"
                )));
            }
        }
        Ok(())
    }
}

pub trait NandComposer: Send + Sync {
    fn validate(&self, revision: &ComponentRevision) -> Result<(), Error>;

    fn includes_ancestors(&self, _component: &ArtifactComponent) -> bool {
        false
    }

    fn check_install(
        &self,
        revision: &ComponentRevision,
        active: &[ComponentRevision],
    ) -> Result<(), Error> {
        if active.iter().any(|old| {
            old.component.id == revision.component.id
                && old.artifact.version() == revision.artifact.version()
        }) {
            return Err(Error::HistoryConflict(
                "this component version is already installed; uninstall it before reinstalling"
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
        Composition::disjoint(revisions)
    }

    fn retained_revision<'a>(
        &self,
        history: &'a [ComponentRevision],
        ids: &[&str],
    ) -> Option<&'a ComponentRevision> {
        history
            .last()
            .filter(|revision| !ids.contains(&revision.artifact.id()))
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
        revisions
            .iter()
            .filter(|revision| {
                titles.contains(&revision.component.title) && !ids.contains(&revision.artifact.id())
            })
            .map(|revision| revision.artifact.id().to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
