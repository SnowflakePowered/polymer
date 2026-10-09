use super::*;

impl StorageKind {
    pub(in crate::storage::git) fn deletion_ref(self, id: Uuid) -> String {
        format!(
            "refs/consoles/deletions/v2/{}/{}",
            self.metadata_name(),
            id.simple()
        )
    }
}

impl PlatformRepository {
    pub fn delete(&self, kind: StorageKind, id: Uuid) -> Result<(), Error> {
        let _guard = self.0.operation.lock();
        let repo = self.0.repository.to_thread_local();
        let marker = kind.deletion_ref(id);
        if GitVolumeStorage::tip_in(&repo, &marker)?.is_none() {
            let target = match GitVolumeStorage::tip_in(&repo, &kind.ref_name(id))? {
                Some(target) => target,
                None => repo
                    .write_blob(b"Polymer pending deletion")
                    .map_err(|error| Error::git("write deletion marker", error))?
                    .detach(),
            };
            repo.reference(
                marker.as_str(),
                target,
                PreviousValue::MustNotExist,
                "Polymer pending deletion",
            )
            .map_err(|error| Error::git("record volume deletion", error))?;
        }
        self.finish_deletion(kind, id)
    }

    pub(super) fn recover_deletions(&self) -> Result<(), Error> {
        let _guard = self.0.operation.lock();
        let repo = self.0.repository.to_thread_local();
        let references = repo
            .references()
            .map_err(|error| Error::git("list deletion markers", error))?;
        let mut pending = Vec::new();
        for reference in references
            .all()
            .map_err(|error| Error::git("list deletion markers", error))?
        {
            let reference = reference.map_err(|error| Error::git("read deletion marker", error))?;
            let name = reference.name().as_bstr().to_str_lossy();
            let Some(suffix) = name.strip_prefix("refs/consoles/deletions/v2/") else {
                continue;
            };
            let Some((kind, id)) = suffix.split_once('/') else {
                return Err(Error::HistoryConflict("invalid deletion marker".into()));
            };
            let kind = StorageKind::ALL
                .into_iter()
                .find(|candidate| candidate.metadata_name() == kind)
                .ok_or_else(|| Error::HistoryConflict("invalid deletion kind".into()))?;
            let id =
                Uuid::parse_str(id).map_err(|error| Error::git("decode deletion volume", error))?;
            pending.push((kind, id));
        }
        for (kind, id) in pending {
            self.finish_deletion(kind, id)?;
        }
        Ok(())
    }

    fn finish_deletion(&self, kind: StorageKind, id: Uuid) -> Result<(), Error> {
        let repo = self.0.repository.to_thread_local();
        let name = kind.ref_name(id);
        let reference = repo
            .try_find_reference(&name)
            .map_err(|error| Error::git("find volume reference", error))?;
        let mut edits = Vec::new();
        if let Some(reference) = reference {
            edits.push(RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::MustExistAndMatch(
                        reference.target().to_owned().into(),
                    ),
                    log: RefLog::AndReference,
                },
                name: reference.name().to_owned(),
                deref: false,
            });
        }
        {
            let profile = id.simple().to_string();
            let (prefixes, mut exact) = if kind == StorageKind::Nand {
                (
                    vec![format!("refs/consoles/components/v2/{profile}/")],
                    vec![format!("refs/consoles/component-transactions/v2/{profile}")],
                )
            } else {
                (
                    vec![
                        format!("refs/consoles/saves/v2/{profile}/"),
                        format!("refs/notes/consoles/v2/{profile}/"),
                    ],
                    vec![
                        format!("refs/consoles/archive/v2/{profile}/recently-deleted"),
                        crate::storage::operation::StorageOperation::journal_ref(id),
                    ],
                )
            };
            exact.extend([kind.recovered_ref(id), kind.recovering_ref(id)]);
            let references = repo
                .references()
                .map_err(|error| Error::git("list profile references", error))?;
            for reference in references
                .all()
                .map_err(|error| Error::git("list profile references", error))?
            {
                let reference =
                    reference.map_err(|error| Error::git("read profile reference", error))?;
                let reference_name = reference.name().as_bstr().to_str_lossy().into_owned();
                if !prefixes
                    .iter()
                    .any(|prefix| reference_name.starts_with(prefix))
                    && !exact.contains(&reference_name)
                {
                    continue;
                }
                edits.push(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::MustExistAndMatch(
                            reference.target().to_owned().into(),
                        ),
                        log: RefLog::AndReference,
                    },
                    name: gix::refs::FullName::try_from(reference_name.as_str())
                        .map_err(|error| Error::git("validate profile reference", error))?,
                    deref: false,
                });
            }
        }
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        repo.edit_references_as(edits, Some(signature.to_ref(&mut time)))
            .map_err(|error| Error::git("delete volume reference", error))?;
        self.volume(kind, id)
            .remove_from_index(repo.object_hash())?;
        let checkout = self.checkout_path(kind, id);
        if checkout.exists() {
            fs::remove_dir_all(&checkout).map_err(|error| Error::io(&checkout, error))?;
        }
        let marker = kind.deletion_ref(id);
        if let Some(reference) = repo
            .try_find_reference(&marker)
            .map_err(|error| Error::git("find deletion marker", error))?
        {
            reference
                .delete()
                .map_err(|error| Error::git("clear deletion marker", error))?;
        }
        Ok(())
    }
}
