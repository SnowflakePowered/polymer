use super::*;
use crate::{ArtifactComponent, ArtifactRevision};

mod composition;
mod operations;
mod revision;
mod snapshot;
mod transaction;

pub(super) struct ComponentHistory<'a> {
    storage: &'a GitVolumeStorage,
    repo: &'a gix::Repository,
}

#[derive(Clone)]
struct ComponentState {
    reference: String,
    tip: gix::ObjectId,
    revision: crate::nand::composition::ComponentRevision,
}

impl GitVolumeStorage {
    pub(super) fn has_component_histories(&self) -> bool {
        self.kind() == StorageKind::Nand && self.inner.platform.0.composer.is_some()
    }

    pub(super) fn recover_components(&self) -> Result<(), Error> {
        if self.has_component_histories() {
            let _guard = self.inner.platform.0.operation.lock();
            let repo = self.inner.platform.repository().to_thread_local();
            ComponentHistory::new(self, &repo).recover()?;
        }
        Ok(())
    }
}

impl<'a> ComponentHistory<'a> {
    pub(super) fn new(storage: &'a GitVolumeStorage, repo: &'a gix::Repository) -> Self {
        Self { storage, repo }
    }

    fn prefix(&self) -> String {
        format!(
            "refs/consoles/components/v2/{}/",
            self.storage.id().simple()
        )
    }

    fn states(&self) -> Result<Vec<ComponentState>, Error> {
        let prefix = self.prefix();
        let references = self
            .repo
            .references()
            .map_err(|error| Error::git("list component references", error))?;
        let mut states = Vec::new();
        for reference in references
            .all()
            .map_err(|error| Error::git("list component references", error))?
        {
            let reference =
                reference.map_err(|error| Error::git("read component reference", error))?;
            let name = reference.name().as_bstr().to_str_lossy().into_owned();
            if !name.starts_with(&prefix) {
                continue;
            }
            let tip = reference
                .try_id()
                .ok_or_else(|| Error::HistoryConflict("symbolic component reference".into()))?
                .detach();
            let state = self.state(name.clone(), tip)?;
            if name != format!("{prefix}{}", TreeKey::new(&state.revision.component.id)) {
                return Err(Error::HistoryConflict(
                    "component reference does not match its root".into(),
                ));
            }
            states.push(state);
        }
        let reference = self.storage.ref_name();
        let installed = ArtifactRepository::new(self.repo, &reference).records()?;
        let installed = installed
            .iter()
            .map(|record| record.id.as_str())
            .collect::<BTreeSet<_>>();
        let contributions = self.contributions(&states)?;
        let composition = self
            .composer()?
            .compose(&contributions, &self.composer_state()?)?;
        let selected = composition
            .artifacts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if installed != selected {
            return Err(Error::HistoryConflict(
                "NAND artifacts do not match its component histories".into(),
            ));
        }
        let root = GitVolumeStorage::root_tree(
            self.repo,
            GitVolumeStorage::tip_in(self.repo, &reference)?,
        )?;
        for state in &states {
            let path = format!(
                "meta/resources/components/{}/head",
                TreeKey::new(&state.revision.component.id)
            );
            let entry = root
                .lookup_entry_by_path(&path)
                .map_err(|error| Error::git("read component selection", error))?
                .ok_or_else(|| Error::HistoryConflict("NAND has no component selection".into()))?;
            let blob = self
                .repo
                .find_blob(entry.oid())
                .map_err(|error| Error::git("read component selection", error))?;
            if blob.data != state.tip.to_string().as_bytes() {
                return Err(Error::HistoryConflict(
                    "NAND component selection differs from its history".into(),
                ));
            }
        }
        Ok(states)
    }

    fn clean_tip(&self) -> Result<gix::ObjectId, Error> {
        self.recover()?;
        let tip = GitVolumeStorage::tip_in(self.repo, &self.storage.ref_name())?
            .ok_or(Error::UnclaimedNand)?;
        let root = GitVolumeStorage::root_tree(self.repo, Some(tip))?;
        if !self.storage.inner.working.read().is_empty()
            || !self.storage.checkout_matches_commit(
                self.repo,
                &root,
                &self.storage.target_index(self.repo, &root)?,
            )?
        {
            return Err(Error::HistoryConflict(
                "NAND has uncommitted changes".into(),
            ));
        }
        Ok(tip)
    }
}
