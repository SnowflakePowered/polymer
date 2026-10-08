use super::*;
use crate::nand::composition::Composition;

impl ComponentHistory<'_> {
    pub(super) fn composer_state(&self) -> Result<serde_json::Value, Error> {
        let root = GitVolumeStorage::root_tree(
            self.repo,
            GitVolumeStorage::tip_in(self.repo, &self.storage.ref_name())?,
        )?;
        let entry = root
            .lookup_entry_by_path("meta/resources/composer-state")
            .map_err(|error| Error::git("read composer state", error))?;
        let Some(entry) = entry else {
            return Ok(serde_json::Value::Null);
        };
        let blob = self
            .repo
            .find_blob(entry.oid())
            .map_err(|error| Error::git("read composer state", error))?;
        serde_json::from_slice(&blob.data)
            .map_err(|error| Error::git("decode composer state", error))
    }

    pub(super) fn sequence(&self, tip: gix::ObjectId) -> Result<u64, Error> {
        let root = GitVolumeStorage::root_tree(self.repo, Some(tip))?;
        let entry = root
            .lookup_entry_by_path("meta/resources/component-sequence")
            .map_err(|error| Error::git("read component sequence", error))?;
        let Some(entry) = entry else { return Ok(0) };
        let blob = self
            .repo
            .find_blob(entry.oid())
            .map_err(|error| Error::git("read component sequence", error))?;
        serde_json::from_slice(&blob.data)
            .map_err(|error| Error::git("decode component sequence", error))
    }

    pub(super) fn compose(
        &self,
        old_tip: gix::ObjectId,
        before: &[ComponentState],
        after: &[ComponentState],
    ) -> Result<gix::ObjectId, Error> {
        let state = self.composer_state()?;
        let previous = self
            .composer()?
            .compose(&self.contributions(before)?, &state)?;
        let contributions = self.contributions(after)?;
        let composition = self.composer()?.compose(&contributions, &state)?;
        composition.validate()?;
        let root = GitVolumeStorage::root_tree(self.repo, Some(old_tip))?;
        self.check_destinations(&root, before, after)?;
        for path in composition.files.keys().chain(composition.generated.keys()) {
            if previous.files.contains_key(path)
                || previous.generated.contains_key(path)
                || before
                    .iter()
                    .any(|state| path.starts_with(&format!("{}/", state.revision.component.root)))
            {
                continue;
            }
            if root
                .lookup_entry_by_path(format!("files/{path}"))
                .map_err(|error| Error::git("check composed destination", error))?
                .is_some()
            {
                return Err(Error::HistoryConflict(format!("unowned files at {path}")));
            }
        }
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("compose NAND", error))?;
        self.clear_composition(&mut editor, &root, before, &previous)?;
        self.write_composition(&mut editor, &composition)?;
        let state = serde_json::to_string(&composition.state)
            .map_err(|error| Error::git("encode composer state", error))?;
        PlatformRepository::write_text(&mut editor, "meta/resources/composer-state", &state)?;
        for state in after {
            PlatformRepository::write_text(
                &mut editor,
                &format!(
                    "meta/resources/components/{}/head",
                    TreeKey::new(&state.revision.component.id)
                ),
                &state.tip.to_string(),
            )?;
        }
        let sequence = self.sequence(old_tip)?.max(
            contributions
                .iter()
                .map(|revision| revision.sequence)
                .max()
                .unwrap_or(0),
        );
        PlatformRepository::write_text(
            &mut editor,
            "meta/resources/component-sequence",
            &sequence.to_string(),
        )?;
        let tree = editor
            .write()
            .map_err(|error| Error::git("write composed NAND", error))?
            .detach();
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let parents = std::iter::once(old_tip).chain(after.iter().map(|state| state.tip));
        self.repo
            .new_commit_as(
                signature,
                signature,
                "Polymer component transition",
                tree,
                parents,
            )
            .map(|commit| commit.id)
            .map_err(|error| Error::git("write composed NAND commit", error))
    }

    fn check_destinations(
        &self,
        root: &gix::Tree<'_>,
        before: &[ComponentState],
        after: &[ComponentState],
    ) -> Result<(), Error> {
        for state in after {
            let path = &state.revision.component.root;
            if before
                .iter()
                .any(|old| old.revision.component.root == *path)
            {
                continue;
            }
            if root
                .lookup_entry_by_path(format!("files/{path}"))
                .map_err(|error| Error::git("check component destination", error))?
                .is_some()
            {
                return Err(Error::HistoryConflict(format!("unowned files at {path}")));
            }
        }
        Ok(())
    }

    fn clear_composition(
        &self,
        editor: &mut gix::object::tree::Editor<'_>,
        root: &gix::Tree<'_>,
        before: &[ComponentState],
        previous: &Composition,
    ) -> Result<(), Error> {
        let roots = before
            .iter()
            .map(|state| &state.revision.component.root)
            .collect::<BTreeSet<_>>();
        for path in &roots {
            editor
                .remove(format!("files/{path}"))
                .map_err(|error| Error::git("remove previous component files", error))?;
        }
        for path in previous.files.keys().chain(previous.generated.keys()) {
            editor
                .remove(format!("files/{path}"))
                .map_err(|error| Error::git("remove previous contribution", error))?;
        }
        for id in previous.artifacts.keys() {
            editor
                .remove(format!("meta/artifacts/{}", TreeKey::new(id)))
                .map_err(|error| Error::git("remove previous component metadata", error))?;
        }
        editor
            .remove("meta/resources/components")
            .map_err(|error| Error::git("replace component selections", error))?;
        for path in self.storage.base_directories(self.repo, root)? {
            if roots
                .iter()
                .any(|root| path == **root || path.starts_with(&format!("{root}/")))
            {
                editor
                    .remove_leaf(format!("meta/fs/directories/{}", TreeKey::new(&path)))
                    .map_err(|error| Error::git("remove component directory", error))?;
            }
        }
        Ok(())
    }

    fn write_composition(
        &self,
        editor: &mut gix::object::tree::Editor<'_>,
        composition: &Composition,
    ) -> Result<(), Error> {
        let mut trees = BTreeMap::new();
        for (path, contribution) in &composition.files {
            GitVolumeStorage::validate(path)?;
            if !trees.contains_key(&contribution.revision) {
                let tip = gix::ObjectId::from_hex(contribution.revision.as_bytes())
                    .map_err(|error| Error::git("parse component contribution", error))?;
                trees.insert(
                    contribution.revision.clone(),
                    GitVolumeStorage::root_tree(self.repo, Some(tip))?,
                );
            }
            let tree = &trees[&contribution.revision];
            let entry = tree
                .lookup_entry_by_path(format!("files/{}", contribution.path))
                .map_err(|error| Error::git("read component contribution", error))?
                .ok_or_else(|| Error::HistoryConflict(format!("missing component file: {path}")))?;
            editor
                .upsert(format!("files/{path}"), entry.mode().kind(), entry.oid())
                .map_err(|error| Error::git("compose component file", error))?;
        }
        for path in &composition.directories {
            GitVolumeStorage::validate(path)?;
            PlatformRepository::write_text(
                editor,
                &format!("meta/fs/directories/{}", TreeKey::new(path)),
                path,
            )?;
        }
        for (path, bytes) in &composition.generated {
            GitVolumeStorage::validate(path)?;
            let blob = self
                .repo
                .write_blob(bytes)
                .map_err(|error| Error::git("write generated contribution", error))?;
            editor
                .upsert(
                    format!("files/{path}"),
                    gix::objs::tree::EntryKind::Blob,
                    blob.detach(),
                )
                .map_err(|error| Error::git("compose generated contribution", error))?;
        }
        for artifact in composition.artifacts.values() {
            ArtifactRecord::from(artifact).write(
                editor,
                &format!("meta/artifacts/{}", TreeKey::new(artifact.id())),
            )?;
        }
        Ok(())
    }
}
