use super::*;
use crate::nand::composition::{ComponentRevision, NandComposer};

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct RevisionMetadata {
    pub sequence: u64,
    pub metadata: serde_json::Value,
}

impl ComponentHistory<'_> {
    pub(super) fn composer(&self) -> Result<&'static dyn NandComposer, Error> {
        self.storage
            .inner
            .platform
            .0
            .composer
            .filter(|_| self.storage.kind() == StorageKind::Nand)
            .ok_or_else(|| {
                Error::Unsupported("component histories are unavailable for this NAND".into())
            })
    }

    pub(super) fn validate(&self, revision: &ComponentRevision) -> Result<(), Error> {
        GitVolumeStorage::validate(&revision.component.root)?;
        if revision.component.id.is_empty() || revision.sequence == 0 {
            return Err(Error::HistoryConflict("invalid component revision".into()));
        }
        let prefix = format!("{}/", revision.component.root);
        for file in revision.artifact.manifest() {
            let path = file.path().to_string_lossy().replace('\\', "/");
            GitVolumeStorage::validate(&path)?;
            if !path.starts_with(&prefix) {
                return Err(Error::HistoryConflict(format!(
                    "component cannot own {path}"
                )));
            }
        }
        for path in &revision.directories {
            GitVolumeStorage::validate(path)?;
            if path != &revision.component.root && !path.starts_with(&prefix) {
                return Err(Error::HistoryConflict(format!(
                    "component cannot own {path}"
                )));
            }
        }
        self.composer()?.validate(revision)
    }

    pub(super) fn state(
        &self,
        reference: String,
        tip: gix::ObjectId,
    ) -> Result<ComponentState, Error> {
        let tree = GitVolumeStorage::root_tree(self.repo, Some(tip))?;
        let component = self.read_metadata::<ArtifactComponent>(&tree, "meta/component")?;
        let metadata = self.read_metadata::<RevisionMetadata>(&tree, "meta/revision")?;
        let mut records = ArtifactRepository::new(self.repo, &reference).records_at(tip)?;
        if records.len() != 1 {
            return Err(Error::HistoryConflict(
                "component revision must contain one artifact".into(),
            ));
        }
        let revision = ComponentRevision {
            component,
            artifact: records.remove(0).artifact()?,
            revision: tip.to_string(),
            sequence: metadata.sequence,
            metadata: metadata.metadata,
            directories: self
                .storage
                .base_directories(self.repo, &tree)?
                .into_iter()
                .collect(),
        };
        self.validate(&revision)?;
        Ok(ComponentState {
            reference,
            tip,
            revision,
        })
    }

    pub(super) fn read_metadata<T: serde::de::DeserializeOwned>(
        &self,
        tree: &gix::Tree<'_>,
        path: &str,
    ) -> Result<T, Error> {
        let entry = tree
            .lookup_entry_by_path(path)
            .map_err(|error| Error::git("read component metadata", error))?
            .ok_or_else(|| Error::HistoryConflict(format!("missing component metadata: {path}")))?;
        let blob = self
            .repo
            .find_blob(entry.oid())
            .map_err(|error| Error::git("read component metadata", error))?;
        serde_json::from_slice(&blob.data)
            .map_err(|error| Error::git("decode component metadata", error))
    }

    pub(super) fn revisions(
        &self,
        state: &ComponentState,
    ) -> Result<Vec<ComponentRevision>, Error> {
        let mut current = state.clone();
        let mut history = Vec::new();
        loop {
            let commit = self
                .repo
                .find_commit(current.tip)
                .map_err(|error| Error::git("read component history", error))?;
            let parents = commit
                .parent_ids()
                .map(|id| id.detach())
                .collect::<Vec<_>>();
            if parents.len() > 1 || current.revision.component != state.revision.component {
                return Err(Error::HistoryConflict("invalid component ancestry".into()));
            }
            let sequence = current.revision.sequence;
            history.push(current.revision);
            let Some(parent) = parents.first() else { break };
            current = self.state(state.reference.clone(), *parent)?;
            if current.revision.sequence >= sequence {
                return Err(Error::HistoryConflict(
                    "component history is out of order".into(),
                ));
            }
        }
        history.reverse();
        Ok(history)
    }

    pub(super) fn contributions(
        &self,
        states: &[ComponentState],
    ) -> Result<Vec<ComponentRevision>, Error> {
        let mut revisions = Vec::new();
        for state in states {
            if self
                .composer()?
                .includes_ancestors(&state.revision.component)
            {
                revisions.extend(self.revisions(state)?);
            } else {
                revisions.push(state.revision.clone());
            }
        }
        revisions.sort_by_key(|revision| revision.sequence);
        if revisions
            .windows(2)
            .any(|pair| pair[0].sequence == pair[1].sequence)
        {
            return Err(Error::HistoryConflict(
                "duplicate component sequence".into(),
            ));
        }
        Ok(revisions)
    }
}
