use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::Path;

use super::{
    AttachmentTips, SaveAttachmentKind, SaveDepotId, SaveRefState, SaveTransition, SaveVersioning,
};
use crate::{Error, SaveRepo};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

struct MaterializationJournal {
    depot: u64,
    old: Option<String>,
    target: Option<String>,
    attachments: AttachmentTips,
}

impl SaveVersioning {
    fn journal_ref(&self) -> String {
        crate::storage::operation::StorageOperation::journal_ref(self.inner.volume_id())
    }

    fn parse_journal_id(value: Option<String>) -> Result<Option<gix::ObjectId>, Error> {
        value
            .map(|value| {
                gix::ObjectId::from_hex(value.as_bytes())
                    .map_err(|error| Error::git("parse transaction object id", error))
            })
            .transpose()
    }

    fn write_journal(&self, transition: SaveTransition) -> Result<(), Error> {
        let after = transition.after;
        let journal = MaterializationJournal {
            depot: transition.depot.0,
            old: transition.before.map(|state| state.tip.to_string()),
            target: after.map(|state| state.tip.to_string()),
            attachments: after.map(|state| state.attachments).unwrap_or_default(),
        };
        self.write_journal_ref(&journal)
    }

    fn read_journal(&self) -> Result<Option<MaterializationJournal>, Error> {
        let repo = self.inner.repository().to_thread_local();
        let Some(tip) = Self::tip_in(&repo, &self.journal_ref())? else {
            return Ok(None);
        };
        let tree = Self::commit_tree(&repo, tip)?;
        let mut values = Self::tree_files(&repo, tree)?;
        let required = |values: &mut BTreeMap<String, Vec<u8>>, name: &str| {
            String::from_utf8(values.remove(name).ok_or_else(|| {
                Error::HistoryConflict(format!("profile transaction has no `{name}`"))
            })?)
            .map_err(|error| Error::git("decode profile transaction", error))
        };
        let optional = |values: &mut BTreeMap<String, Vec<u8>>, name: &str| {
            values
                .remove(name)
                .map(String::from_utf8)
                .transpose()
                .map_err(|error| Error::git("decode profile transaction", error))
        };
        Ok(Some(MaterializationJournal {
            depot: required(&mut values, "depot")?
                .parse()
                .map_err(|error| Error::git("parse profile transaction depot", error))?,
            old: optional(&mut values, "old")?,
            target: optional(&mut values, "target")?,
            attachments: AttachmentTips::collect(|kind| {
                Self::parse_journal_id(optional(&mut values, kind.as_str())?)
            })?,
        }))
    }

    fn write_journal_ref(&self, journal: &MaterializationJournal) -> Result<(), Error> {
        let repo = self.inner.repository().to_thread_local();
        let reference = self.journal_ref();
        let mut editor = repo
            .empty_tree()
            .edit()
            .map_err(|error| Error::git("edit profile transaction", error))?;
        let values = [
            ("depot", Some(journal.depot.to_string())),
            ("old", journal.old.clone()),
            ("target", journal.target.clone()),
        ]
        .into_iter()
        .chain(SaveAttachmentKind::ALL.into_iter().map(|kind| {
            (
                kind.as_str(),
                journal.attachments.get(kind).map(|id| id.to_string()),
            )
        }));
        for (name, value) in values {
            let Some(value) = value else { continue };
            let blob = repo
                .write_blob(value.as_bytes())
                .map_err(|error| Error::git("write profile transaction value", error))?
                .detach();
            editor
                .upsert(name, gix::objs::tree::EntryKind::Blob, blob)
                .map_err(|error| Error::git("update profile transaction", error))?;
        }
        let tree = editor
            .write()
            .map_err(|error| Error::git("write profile transaction tree", error))?
            .detach();
        let signature = Self::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let commit = repo
            .new_commit_as(
                signature,
                signature,
                "Polymer profile materialization transaction",
                tree,
                std::iter::empty::<gix::ObjectId>(),
            )
            .map_err(|error| Error::git("commit profile transaction", error))?
            .id;
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: "polymer: materialization transaction".into(),
                },
                expected: PreviousValue::MustNotExist,
                new: gix::refs::Target::Object(commit),
            },
            name: gix::refs::FullName::try_from(reference.as_str())
                .map_err(|error| Error::git("validate profile transaction ref", error))?,
            deref: false,
        };
        repo.edit_references_as([edit], Some(signature))
            .map(|_| ())
            .map_err(|error| Error::git("publish profile transaction", error))
    }

    pub(super) fn ensure_no_pending_journal(&self) -> Result<(), Error> {
        self.inner.operation.ensure_ready()?;
        self.ensure_no_materialization()
    }

    pub(super) fn ensure_no_materialization(&self) -> Result<(), Error> {
        let repo = self.inner.repository().to_thread_local();
        if Self::tip_in(&repo, &self.journal_ref())?.is_some() {
            return Err(Error::HistoryConflict(
                "save materialization requires recovery".to_owned(),
            ));
        }
        Ok(())
    }

    fn clear_journal(&self) -> Result<(), Error> {
        let repo = self.inner.repository().to_thread_local();
        let reference = self.journal_ref();
        let Some(existing) = repo
            .try_find_reference(&reference)
            .map_err(|error| Error::git("find profile transaction", error))?
        else {
            return Ok(());
        };
        let edit = RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExistAndMatch(existing.target().to_owned().into()),
                log: RefLog::AndReference,
            },
            name: gix::refs::FullName::try_from(reference.as_str())
                .map_err(|error| Error::git("validate profile transaction ref", error))?,
            deref: false,
        };
        let signature = Self::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        repo.edit_references_as([edit], Some(signature.to_ref(&mut time)))
            .map(|_| ())
            .map_err(|error| Error::git("clear profile transaction", error))
    }

    pub(super) async fn recover_locked<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
    ) -> Result<(), Error> {
        self.inner.operation.ensure_ready()?;
        let Some(journal) = self.read_journal()? else {
            return Ok(());
        };
        let transition = SaveTransition {
            depot: SaveDepotId(journal.depot),
            before: Self::parse_journal_id(journal.old)?.map(|tip| SaveRefState {
                tip,
                attachments: AttachmentTips::default(),
            }),
            after: Self::parse_journal_id(journal.target)?
                .map(|tip| -> Result<_, Error> {
                    Ok(SaveRefState {
                        tip,
                        attachments: journal.attachments,
                    })
                })
                .transpose()?,
        };
        let (old_files, new_files) = {
            let repo = self.inner.repository().to_thread_local();
            (
                transition
                    .before
                    .map(|state| Self::commit_files(&repo, state.tip))
                    .transpose()?
                    .unwrap_or_default(),
                transition
                    .after
                    .map(|state| Self::commit_files(&repo, state.tip))
                    .transpose()?
                    .unwrap_or_default(),
            )
        };
        Self::apply_files(owner, &old_files, &new_files).await?;
        let repo = self.inner.repository().to_thread_local();
        self.install_save_refs(&repo, transition.depot, transition.after)?;
        self.inner
            .profile
            .set_save_tip(transition.depot.0, transition.after.map(|state| state.tip));
        self.inner
            .profile
            .commit_pending("Polymer profile save transition")?;
        self.clear_journal()
    }

    pub(super) async fn apply_transition_locked<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        transition: SaveTransition,
    ) -> Result<(), Error> {
        self.ensure_no_pending_journal()?;
        self.write_journal(transition)?;
        self.recover_locked(owner).await
    }

    pub(crate) async fn recover<R: SaveRepo + ?Sized>(&self, owner: &R) -> Result<(), Error> {
        let _guard = self.inner.operation.gate.lock().await;
        self.recover_locked(owner).await
    }

    async fn apply_files<R: crate::Storage + ?Sized>(
        owner: &R,
        old: &BTreeMap<String, Vec<u8>>,
        new: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        for (path, bytes) in new {
            owner
                .install_file(Box::new(Cursor::new(bytes.clone())), path)
                .await?;
        }
        for path in old.keys().filter(|path| !new.contains_key(*path)) {
            match owner.remove(path) {
                Ok(()) => {}
                Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }

        let mut directories = BTreeSet::new();
        for path in old.keys() {
            let mut parent = Path::new(path).parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                directories.insert(path.to_string_lossy().replace('\\', "/"));
                parent = path.parent();
            }
        }
        for directory in directories.into_iter().rev() {
            if owner
                .read_dir(&directory, false)
                .is_ok_and(|mut entries| entries.next().is_none())
            {
                owner.remove(&directory)?;
            }
        }
        Ok(())
    }
}
