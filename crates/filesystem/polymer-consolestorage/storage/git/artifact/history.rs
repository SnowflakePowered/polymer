use super::*;

impl ArtifactRepository<'_> {
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

    pub(in crate::storage::git) fn linear_history(
        &self,
        tip: gix::ObjectId,
    ) -> Result<Vec<NandCommit>, Error> {
        let mut current = Some(tip);
        let mut history = Vec::new();
        while let Some(id) = current {
            let object = self
                .repo
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

    fn tree_snapshot(&self, tree: gix::ObjectId) -> Result<BTreeMap<String, TreeLeaf>, Error> {
        let tree = self
            .repo
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

    fn parent_snapshot(&self, commit: &NandCommit) -> Result<BTreeMap<String, TreeLeaf>, Error> {
        match commit.commit.parents.first() {
            Some(parent) => self
                .repo
                .find_commit(*parent)
                .map_err(|error| Error::git("read NAND parent", error))?
                .tree_id()
                .map_err(|error| Error::git("read NAND parent tree", error))
                .and_then(|tree| self.tree_snapshot(tree.detach())),
            None => Ok(Self::empty_snapshot()),
        }
    }

    fn apply_delta(
        &self,
        commit: &NandCommit,
        current: &mut BTreeMap<String, TreeLeaf>,
        removed: &BTreeSet<String>,
    ) -> Result<Vec<String>, Error> {
        let removed_metadata = removed
            .iter()
            .map(|id| format!("meta/artifacts/{}/", TreeKey::new(id)))
            .collect::<Vec<_>>();
        let before = self.parent_snapshot(commit)?;
        let after = self.tree_snapshot(commit.commit.tree)?;
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
        &self,
        snapshot: &BTreeMap<String, TreeLeaf>,
    ) -> Result<gix::ObjectId, Error> {
        let mut editor = self
            .repo
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
    pub(in crate::storage::git) fn retained_files(
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
        &self,
        commit: &NandCommit,
        current: &mut BTreeMap<String, TreeLeaf>,
        retained: &BTreeSet<String>,
    ) -> Result<(), Error> {
        let before = self.parent_snapshot(commit)?;
        let after = self.tree_snapshot(commit.commit.tree)?;
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

    pub(in crate::storage::git) fn affected_depots(
        &self,
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
            let mut current = self.parent_snapshot(&history[first])?;
            for commit in &history[first..] {
                if commit
                    .artifact
                    .as_ref()
                    .is_some_and(|id| selected.contains(id))
                {
                    self.retain_delta(commit, &mut current, &retained)?;
                    continue;
                }
                let conflicts = self.apply_delta(commit, &mut current, &selected)?;
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

    pub(in crate::storage::git) fn rewrite_without(
        &self,
        history: &[NandCommit],
        removed: &BTreeSet<String>,
        retained: &BTreeSet<String>,
        old_tip: gix::ObjectId,
    ) -> Result<gix::ObjectId, Error> {
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
        let mut current = self.parent_snapshot(&history[first])?;
        let mut parent = history[first].commit.parents.first().copied();
        for original in &history[first..] {
            if original
                .artifact
                .as_ref()
                .is_some_and(|id| removed.contains(id))
            {
                self.retain_delta(original, &mut current, retained)?;
                continue;
            }
            let conflicts = self.apply_delta(original, &mut current, removed)?;
            if !conflicts.is_empty() {
                return Err(Error::HistoryConflict(format!(
                    "rewriting commit {} conflicts at {}",
                    original.id,
                    conflicts.join(", ")
                )));
            }
            let mut rewritten = original.commit.clone();
            rewritten.tree = self.write_snapshot(&current)?;
            rewritten.parents.clear();
            if let Some(parent) = parent {
                rewritten.parents.push(parent);
            }
            parent = Some(
                self.repo
                    .write_object(rewritten)
                    .map_err(|error| Error::git("write rebased NAND commit", error))?
                    .detach(),
            );
        }
        let new_tip = parent.ok_or_else(|| {
            Error::HistoryConflict("uninstall cannot remove the entire NAND history".to_owned())
        })?;
        let rewritten = GitVolumeStorage::root_tree(self.repo, Some(new_tip))?;
        let remaining = self
            .trees(&rewritten)?
            .iter()
            .map(|(_, tree)| tree.summary())
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(artifact) = remaining
            .iter()
            .find(|artifact| removed.contains(&artifact.id))
        {
            return Err(Error::HistoryConflict(format!(
                "rewritten NAND history still records {}",
                artifact.id
            )));
        }
        let name = gix::refs::FullName::try_from(self.reference)
            .map_err(|error| Error::git("validate NAND reference", error))?;
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        self.repo
            .edit_references_as(
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
        self.repo
            .edit_references_as(
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
        Ok(new_tip)
    }
}
