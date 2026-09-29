use super::*;

impl GitArtifactRegistry {
    pub(super) fn storage(&self) -> GitVolumeStorage {
        GitVolumeStorage {
            inner: self.0.clone(),
            artifacts: self.clone(),
        }
    }

    pub(super) fn records(&self) -> Result<Vec<ArtifactRecord>, Error> {
        self.read_artifacts(read_artifact)
    }

    pub(super) fn read_artifacts<T>(
        &self,
        read: impl Fn(&gix::Repository, &gix::Tree<'_>) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let repo = self.0.platform.repository().to_thread_local();
        let tip = GitVolumeStorage::tip_in(&repo, &self.storage().ref_name())?;
        Self::artifacts_in(&repo, &GitVolumeStorage::root_tree(&repo, tip)?, read)
    }

    fn artifacts_in<T>(
        repo: &gix::Repository,
        tree: &gix::Tree<'_>,
        read: impl Fn(&gix::Repository, &gix::Tree<'_>) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let Some(artifacts) = tree
            .lookup_entry_by_path("meta/artifacts")
            .map_err(|error| Error::git("lookup artifacts", error))?
        else {
            return Ok(Vec::new());
        };
        let artifacts = repo
            .find_tree(artifacts.oid())
            .map_err(|error| Error::git("read artifacts", error))?;
        let mut records = Vec::new();
        for entry in artifacts.iter() {
            let entry = entry.map_err(|error| Error::git("decode artifact entry", error))?;
            if entry.kind() != gix::objs::tree::EntryKind::Tree {
                continue;
            }
            let tree = repo
                .find_tree(entry.oid())
                .map_err(|error| Error::git("read artifact tree", error))?;
            records.push(read(repo, &tree)?);
        }
        Ok(records)
    }

    fn artifact_from_message(message: &[u8]) -> Option<String> {
        message
            .as_bstr()
            .to_str_lossy()
            .lines()
            .next()?
            .strip_prefix("Polymer install ")
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    }

    pub(super) fn linear_history(
        repo: &gix::Repository,
        tip: gix::ObjectId,
    ) -> Result<Vec<NandCommit>, Error> {
        let mut current = Some(tip);
        let mut history = Vec::new();
        while let Some(id) = current {
            let object = repo
                .find_commit(id)
                .map_err(|error| Error::git("read NAND commit", error))?;
            let decoded = object
                .decode()
                .map_err(|error| Error::git("decode NAND commit", error))?;
            let commit = gix::objs::Commit::try_from(decoded)
                .map_err(|error| Error::git("own NAND commit", error))?;
            if commit.parents.len() > 1 {
                return Err(Error::HistoryConflict(
                    "NAND history must be linear to uninstall depots".to_owned(),
                ));
            }
            current = commit.parents.first().copied();
            history.push(NandCommit {
                id,
                artifact: Self::artifact_from_message(&commit.message),
                commit,
            });
        }
        history.reverse();
        Ok(history)
    }

    fn tree_snapshot(
        repo: &gix::Repository,
        tree: gix::ObjectId,
    ) -> Result<BTreeMap<String, TreeLeaf>, Error> {
        let tree = repo
            .find_tree(tree)
            .map_err(|error| Error::git("read NAND tree", error))?;
        let entries = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|error| Error::git("traverse NAND tree", error))?;
        let mut snapshot = BTreeMap::new();
        for entry in entries {
            let kind = entry.mode.kind();
            if kind == gix::objs::tree::EntryKind::Tree {
                continue;
            }
            if !matches!(
                kind,
                gix::objs::tree::EntryKind::Blob | gix::objs::tree::EntryKind::BlobExecutable
            ) {
                return Err(Error::HistoryConflict(format!(
                    "unsupported entry in NAND tree: {}",
                    entry.filepath.to_str_lossy()
                )));
            }
            snapshot.insert(
                entry.filepath.to_str_lossy().into_owned(),
                TreeLeaf {
                    kind,
                    id: entry.oid,
                },
            );
        }
        Ok(snapshot)
    }

    fn empty_snapshot() -> BTreeMap<String, TreeLeaf> {
        BTreeMap::new()
    }

    fn parent_snapshot(
        repo: &gix::Repository,
        commit: &NandCommit,
    ) -> Result<BTreeMap<String, TreeLeaf>, Error> {
        match commit.commit.parents.first() {
            Some(parent) => repo
                .find_commit(*parent)
                .map_err(|error| Error::git("read NAND parent", error))?
                .tree_id()
                .map_err(|error| Error::git("read NAND parent tree", error))
                .and_then(|tree| Self::tree_snapshot(repo, tree.detach())),
            None => Ok(Self::empty_snapshot()),
        }
    }

    fn apply_delta(
        repo: &gix::Repository,
        commit: &NandCommit,
        current: &mut BTreeMap<String, TreeLeaf>,
        removed: &BTreeSet<String>,
    ) -> Result<Vec<String>, Error> {
        let removed_metadata = removed
            .iter()
            .map(|id| format!("meta/artifacts/{}/", artifact_key(id)))
            .collect::<Vec<_>>();
        let before = Self::parent_snapshot(repo, commit)?;
        let after = Self::tree_snapshot(repo, commit.commit.tree)?;
        let paths = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut updated = current.clone();
        let mut conflicts = Vec::new();
        for path in paths {
            let before = before.get(&path).copied();
            let after = after.get(&path).copied();
            if before == after
                || removed_metadata
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            {
                continue;
            }
            let found = current.get(&path).copied();
            if found != before && found != after {
                conflicts.push(path);
                continue;
            }
            match after {
                Some(entry) => {
                    updated.insert(path, entry);
                }
                None => {
                    updated.remove(&path);
                }
            }
        }
        if conflicts.is_empty() {
            *current = updated;
        }
        Ok(conflicts)
    }

    fn write_snapshot(
        repo: &gix::Repository,
        snapshot: &BTreeMap<String, TreeLeaf>,
    ) -> Result<gix::ObjectId, Error> {
        let mut editor = repo
            .empty_tree()
            .edit()
            .map_err(|error| Error::git("edit rewritten NAND tree", error))?;
        for (path, entry) in snapshot {
            editor
                .upsert(path, entry.kind, entry.id)
                .map_err(|error| Error::git("update rewritten NAND tree", error))?;
        }
        editor
            .write()
            .map(|id| id.detach())
            .map_err(|error| Error::git("write rewritten NAND tree", error))
    }

    /// Titles that list a path the selected titles also own, with different content.
    /// Identical content at the same path is shared content: it stays installed for the
    /// remaining owners and never forces them out.
    fn manifest_dependencies(
        records: &[ArtifactRecord],
        selected: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let selected_files = records
            .iter()
            .filter(|record| selected.contains(&record.id))
            .flat_map(|record| record.manifest.iter())
            .map(|file| (file.path.to_string_lossy().replace('\\', "/"), file.blake3))
            .collect::<BTreeMap<_, _>>();
        records
            .iter()
            .filter(|record| !selected.contains(&record.id))
            .filter(|record| {
                record.manifest.iter().any(|file| {
                    selected_files
                        .get(&*file.path.to_string_lossy().replace('\\', "/"))
                        .is_some_and(|hash| *hash != file.blake3)
                })
            })
            .map(|record| record.id.clone())
            .collect()
    }

    /// Volume paths of every file still listed by a title that is not being removed.
    pub(super) fn retained_files(
        records: &[ArtifactRecord],
        removed: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        records
            .iter()
            .filter(|record| !removed.contains(&record.id))
            .flat_map(|record| record.manifest.iter())
            .map(|file| format!("files/{}", file.path.to_string_lossy().replace('\\', "/")))
            .collect()
    }

    /// Keep a dropped install commit's changes to files that remaining titles still list.
    fn retain_delta(
        repo: &gix::Repository,
        commit: &NandCommit,
        current: &mut BTreeMap<String, TreeLeaf>,
        retained: &BTreeSet<String>,
    ) -> Result<(), Error> {
        let before = Self::parent_snapshot(repo, commit)?;
        let after = Self::tree_snapshot(repo, commit.commit.tree)?;
        for path in retained {
            match (before.get(path), after.get(path)) {
                (before, after) if before == after => {}
                (_, Some(entry)) => {
                    current.insert(path.clone(), *entry);
                }
                (_, None) => {
                    current.remove(path);
                }
            }
        }
        Ok(())
    }

    pub(super) fn affected_depots(
        repo: &gix::Repository,
        history: &[NandCommit],
        records: &[ArtifactRecord],
        requested: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, Error> {
        let installed = records
            .iter()
            .map(|record| record.id.clone())
            .collect::<BTreeSet<_>>();
        let mut selected = requested
            .intersection(&installed)
            .cloned()
            .collect::<BTreeSet<_>>();
        if selected.is_empty() {
            return Ok(BTreeSet::new());
        }
        loop {
            let mut added = Self::manifest_dependencies(records, &selected);
            let retained = Self::retained_files(records, &selected);
            let first = history
                .iter()
                .position(|commit| {
                    commit
                        .artifact
                        .as_ref()
                        .is_some_and(|id| selected.contains(id))
                })
                .ok_or_else(|| {
                    Error::HistoryConflict(format!(
                        "no installation commit was found for {}",
                        selected.iter().cloned().collect::<Vec<_>>().join(", ")
                    ))
                })?;
            for id in &selected {
                if !history
                    .iter()
                    .any(|commit| commit.artifact.as_ref() == Some(id))
                {
                    return Err(Error::HistoryConflict(format!(
                        "no installation commit was found for {id}"
                    )));
                }
            }
            let mut current = Self::parent_snapshot(repo, &history[first])?;
            for commit in &history[first..] {
                if commit
                    .artifact
                    .as_ref()
                    .is_some_and(|id| selected.contains(id))
                {
                    Self::retain_delta(repo, commit, &mut current, &retained)?;
                    continue;
                }
                let conflicts = Self::apply_delta(repo, commit, &mut current, &selected)?;
                if conflicts.is_empty() {
                    continue;
                }
                if let Some(id) = &commit.artifact {
                    added.insert(id.clone());
                    break;
                } else {
                    return Err(Error::HistoryConflict(format!(
                        "dropping depot commits conflicts with commit {} at {}",
                        commit.id,
                        conflicts.join(", ")
                    )));
                }
            }
            added.retain(|id| !selected.contains(id));
            if added.is_empty() {
                return Ok(selected.difference(requested).cloned().collect());
            }
            selected.extend(added);
        }
    }

    pub(super) fn rewrite_without(
        &self,
        repo: &gix::Repository,
        history: &[NandCommit],
        removed: &BTreeSet<String>,
        retained: &BTreeSet<String>,
        old_tip: gix::ObjectId,
    ) -> Result<(), Error> {
        let first = history
            .iter()
            .position(|commit| {
                commit
                    .artifact
                    .as_ref()
                    .is_some_and(|id| removed.contains(id))
            })
            .ok_or_else(|| {
                Error::HistoryConflict("no installation commits were selected".to_owned())
            })?;
        let mut current = Self::parent_snapshot(repo, &history[first])?;
        let mut parent = history[first].commit.parents.first().copied();
        for original in &history[first..] {
            if original
                .artifact
                .as_ref()
                .is_some_and(|id| removed.contains(id))
            {
                Self::retain_delta(repo, original, &mut current, retained)?;
                continue;
            }
            let conflicts = Self::apply_delta(repo, original, &mut current, removed)?;
            if !conflicts.is_empty() {
                return Err(Error::HistoryConflict(format!(
                    "rewriting commit {} conflicts at {}",
                    original.id,
                    conflicts.join(", ")
                )));
            }
            let mut rewritten = original.commit.clone();
            rewritten.tree = Self::write_snapshot(repo, &current)?;
            rewritten.parents.clear();
            if let Some(parent) = parent {
                rewritten.parents.push(parent);
            }
            parent = Some(
                repo.write_object(rewritten)
                    .map_err(|error| Error::git("write rebased NAND commit", error))?
                    .detach(),
            );
        }
        let new_tip = parent.ok_or_else(|| {
            Error::HistoryConflict("uninstall cannot remove the entire NAND history".to_owned())
        })?;
        let rewritten = GitVolumeStorage::root_tree(repo, Some(new_tip))?;
        let remaining = Self::artifacts_in(repo, &rewritten, read_artifact_summary)?;
        if let Some(artifact) = remaining
            .iter()
            .find(|artifact| removed.contains(&artifact.id))
        {
            return Err(Error::HistoryConflict(format!(
                "rewritten NAND history still records {}",
                artifact.id
            )));
        }
        let reference = self.storage().ref_name();
        let name = gix::refs::FullName::try_from(reference.as_str())
            .map_err(|error| Error::git("validate NAND reference", error))?;
        let signature = signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        repo.edit_references_as(
            [RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: true,
                        message: format!(
                            "Polymer uninstall {}",
                            removed.iter().cloned().collect::<Vec<_>>().join(" ")
                        )
                        .into(),
                    },
                    expected: PreviousValue::MustExistAndMatch(Target::Object(old_tip)),
                    new: Target::Object(new_tip),
                },
                name: name.clone(),
                deref: false,
            }],
            Some(signature),
        )
        .map_err(|error| Error::git("publish rewritten NAND history", error))?;
        repo.edit_references_as(
            [RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::MustExistAndMatch(Target::Object(new_tip)),
                    log: RefLog::Only,
                },
                name,
                deref: false,
            }],
            Some(signature),
        )
        .map_err(|error| Error::git("clear rewritten NAND reflog", error))?;
        self.storage()
            .refresh_checkout_locked(repo, Some(new_tip))?;
        Ok(())
    }
}
