use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
struct ReferenceChange {
    name: String,
    before: Option<String>,
    after: Option<String>,
}

impl ComponentHistory<'_> {
    pub(in crate::storage::git) fn journal_ref(&self) -> String {
        format!(
            "refs/consoles/component-transactions/v2/{}",
            self.storage.id().simple()
        )
    }

    pub(super) fn transition(
        &self,
        old_tip: gix::ObjectId,
        before: &[ComponentState],
        after: &[ComponentState],
    ) -> Result<(), Error> {
        let new_tip = self.compose(old_tip, before, after)?;
        let mut changes = Vec::new();
        let references = before
            .iter()
            .chain(after)
            .map(|state| &state.reference)
            .collect::<BTreeSet<_>>();
        for name in references {
            let old = before
                .iter()
                .find(|state| &state.reference == name)
                .map(|state| state.tip.to_string());
            let new = after
                .iter()
                .find(|state| &state.reference == name)
                .map(|state| state.tip.to_string());
            if old != new {
                changes.push(ReferenceChange {
                    name: name.clone(),
                    before: old,
                    after: new,
                });
            }
        }
        changes.push(ReferenceChange {
            name: self.storage.ref_name(),
            before: Some(old_tip.to_string()),
            after: Some(new_tip.to_string()),
        });
        let mut editor = self
            .repo
            .empty_tree()
            .edit()
            .map_err(|error| Error::git("create component transaction", error))?;
        let json = serde_json::to_string(&changes)
            .map_err(|error| Error::git("encode component transaction", error))?;
        PlatformRepository::write_text(&mut editor, "transaction", &json)?;
        let tree = editor
            .write()
            .map_err(|error| Error::git("write component transaction", error))?
            .detach();
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let journal = self
            .repo
            .new_commit_as(
                signature,
                signature,
                "Polymer pending component transition",
                tree,
                [old_tip, new_tip],
            )
            .map_err(|error| Error::git("write component transaction commit", error))?
            .id;
        self.update_reference(&self.journal_ref(), None, Some(journal))?;
        *self.storage.inner.cache_ready.lock() = false;
        self.recover()
    }

    fn update_reference(
        &self,
        name: &str,
        before: Option<gix::ObjectId>,
        after: Option<gix::ObjectId>,
    ) -> Result<(), Error> {
        let expected = before.map_or(PreviousValue::MustNotExist, |id| {
            PreviousValue::MustExistAndMatch(Target::Object(id))
        });
        let change = match after {
            Some(id) => Change::Update {
                expected,
                new: Target::Object(id),
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: "Polymer component transition".into(),
                },
            },
            None => Change::Delete {
                expected,
                log: RefLog::AndReference,
            },
        };
        let name = gix::refs::FullName::try_from(name)
            .map_err(|error| Error::git("validate component reference", error))?;
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        self.repo
            .edit_references_as(
                [RefEdit {
                    name,
                    change,
                    deref: false,
                }],
                Some(signature.to_ref(&mut time)),
            )
            .map_err(|error| Error::git("publish component reference", error))?;
        Ok(())
    }

    pub(in crate::storage::git) fn recover(&self) -> Result<(), Error> {
        if GitVolumeStorage::tip_in(
            self.repo,
            &self.storage.kind().deletion_ref(self.storage.id()),
        )?
        .is_some()
        {
            return Err(Error::HistoryConflict("NAND deletion is pending".into()));
        }
        let name = self.journal_ref();
        let Some(journal) = GitVolumeStorage::tip_in(self.repo, &name)? else {
            return Ok(());
        };
        if !self.storage.inner.working.read().is_empty() {
            return Err(Error::HistoryConflict(
                "component recovery requires an empty working overlay".into(),
            ));
        }
        let tree = GitVolumeStorage::root_tree(self.repo, Some(journal))?;
        let entry = tree
            .lookup_entry_by_path("transaction")
            .map_err(|error| Error::git("read component transaction", error))?
            .ok_or_else(|| Error::HistoryConflict("component transaction is missing".into()))?;
        let blob = self
            .repo
            .find_blob(entry.oid())
            .map_err(|error| Error::git("read component transaction", error))?;
        let changes: Vec<ReferenceChange> = serde_json::from_slice(&blob.data)
            .map_err(|error| Error::git("decode component transaction", error))?;
        let volume = self.storage.ref_name();
        let target = changes
            .last()
            .filter(|change| change.name == volume)
            .and_then(|change| change.after.as_deref())
            .ok_or_else(|| {
                Error::HistoryConflict("component transaction has no NAND target".into())
            })?;
        let target = gix::ObjectId::from_hex(target.as_bytes())
            .map_err(|error| Error::git("decode NAND target", error))?;
        for change in &changes {
            self.recover_reference(change)?;
        }
        *self.storage.inner.cache_ready.lock() = false;
        self.storage
            .refresh_checkout_locked(self.repo, Some(target))?;
        self.update_reference(&name, Some(journal), None)
    }

    fn recover_reference(&self, change: &ReferenceChange) -> Result<(), Error> {
        if change.name != self.storage.ref_name() && !change.name.starts_with(&self.prefix()) {
            return Err(Error::HistoryConflict(
                "component transaction references another volume".into(),
            ));
        }
        let parse = |value: &Option<String>| {
            value
                .as_ref()
                .map(|value| {
                    gix::ObjectId::from_hex(value.as_bytes())
                        .map_err(|error| Error::git("decode component revision", error))
                })
                .transpose()
        };
        let before = parse(&change.before)?;
        let after = parse(&change.after)?;
        let current = GitVolumeStorage::tip_in(self.repo, &change.name)?;
        if current == after {
            return Ok(());
        }
        if current != before {
            return Err(Error::HistoryConflict(format!(
                "component transaction conflicts with {}",
                change.name
            )));
        }
        self.update_reference(&change.name, before, after)
    }
}
