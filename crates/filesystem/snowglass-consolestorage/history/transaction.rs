use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::Path;

use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
use serde::{Deserialize, Serialize};

use super::{AttachmentTips, SaveDepotId, SaveRefState, SaveTransition, SaveVersioning};
use crate::{Error, SaveRepo};

#[derive(Serialize, Deserialize)]
struct MaterializationJournal {
    depot: u64,
    old: Option<String>,
    target: Option<String>,
    screenshot: Option<String>,
    icon: Option<String>,
    note: Option<String>,
}

enum MaterializationFailure {
    RolledBack(Error),
    RollbackFailed { apply: Error, rollback: Error },
}

impl SaveVersioning {
    fn journal_ref(&self) -> String {
        format!(
            "refs/snowglass/transactions/v2/{}",
            self.inner.volume_id().simple()
        )
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
            screenshot: after
                .and_then(|state| state.attachments.screenshot)
                .map(|id| id.to_string()),
            icon: after
                .and_then(|state| state.attachments.icon)
                .map(|id| id.to_string()),
            note: after
                .and_then(|state| state.attachments.note)
                .map(|id| id.to_string()),
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
            screenshot: optional(&mut values, "screenshot")?,
            icon: optional(&mut values, "icon")?,
            note: optional(&mut values, "note")?,
        }))
    }

    fn write_journal_ref(&self, journal: &MaterializationJournal) -> Result<(), Error> {
        let repo = self.inner.repository().to_thread_local();
        let reference = self.journal_ref();
        let parent = Self::tip_in(&repo, &reference)?;
        let mut editor = repo
            .empty_tree()
            .edit()
            .map_err(|error| Error::git("edit profile transaction", error))?;
        let depot = journal.depot.to_string();
        let values = [
            ("depot", Some(depot.as_str())),
            ("old", journal.old.as_deref()),
            ("target", journal.target.as_deref()),
            ("screenshot", journal.screenshot.as_deref()),
            ("icon", journal.icon.as_deref()),
            ("note", journal.note.as_deref()),
        ];
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
        repo.commit_as(
            signature,
            signature,
            reference,
            "Snowglass profile materialization transaction",
            tree,
            parent,
        )
        .map(|_| ())
        .map_err(|error| Error::git("commit profile transaction", error))
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
                        attachments: AttachmentTips {
                            screenshot: Self::parse_journal_id(journal.screenshot)?,
                            icon: Self::parse_journal_id(journal.icon)?,
                            note: Self::parse_journal_id(journal.note)?,
                        },
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
        if let Err(failure) = Self::apply_files(owner, &old_files, &new_files).await {
            return match failure {
                MaterializationFailure::RolledBack(error) => {
                    self.clear_journal()?;
                    Err(error)
                }
                MaterializationFailure::RollbackFailed { apply, rollback } => {
                    // Keep the journal. A later recovery will idempotently
                    // roll forward to the requested snapshot.
                    Err(Error::HistoryConflict(format!(
                        "save materialization failed ({apply}); rollback also failed ({rollback})"
                    )))
                }
            };
        }
        let repo = self.inner.repository().to_thread_local();
        if let Err(reference_error) =
            self.install_save_refs(&repo, transition.depot, transition.after)
        {
            drop(repo);
            return match Self::apply_files(owner, &new_files, &old_files).await {
                Ok(()) => {
                    self.clear_journal()?;
                    Err(reference_error)
                }
                Err(MaterializationFailure::RolledBack(rollback)) => {
                    Err(Error::HistoryConflict(format!(
                        "save reference publication failed ({reference_error}); canonical rollback failed ({rollback})"
                    )))
                }
                Err(MaterializationFailure::RollbackFailed { apply, rollback }) => {
                    Err(Error::HistoryConflict(format!(
                        "save reference publication failed ({reference_error}); canonical rollback failed ({apply}) and compensation failed ({rollback})"
                    )))
                }
            };
        }
        self.inner
            .profile
            .set_save_tip(transition.depot.0, transition.after.map(|state| state.tip));
        self.inner
            .profile
            .commit_pending("Snowglass profile save transition", None)?;
        self.clear_journal()
    }

    pub(super) async fn apply_transition_locked<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        transition: SaveTransition,
    ) -> Result<(), Error> {
        self.write_journal(transition)?;
        self.recover_locked(owner).await
    }

    pub(super) async fn recover<R: SaveRepo + ?Sized>(&self, owner: &R) -> Result<(), Error> {
        let _guard = self.inner.operation.lock().await;
        self.recover_locked(owner).await
    }

    async fn apply_files<R: crate::Storage + ?Sized>(
        owner: &R,
        old: &BTreeMap<String, Vec<u8>>,
        new: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), MaterializationFailure> {
        if let Err(apply) = Self::apply_files_once(owner, old, new).await {
            return match Self::apply_files_once(owner, new, old).await {
                Ok(()) => Err(MaterializationFailure::RolledBack(apply)),
                Err(rollback) => Err(MaterializationFailure::RollbackFailed { apply, rollback }),
            };
        }
        Ok(())
    }

    async fn apply_files_once<R: crate::Storage + ?Sized>(
        owner: &R,
        old: &BTreeMap<String, Vec<u8>>,
        new: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        // Install replacements before removing obsolete files. Each install is
        // itself atomic, and this ordering leaves the complete old snapshot
        // available for compensation until every target file is durable.
        for (path, bytes) in new {
            owner
                .install_file(Box::new(Cursor::new(bytes.clone())), path)
                .await?;
        }
        for path in old.keys().filter(|path| !new.contains_key(*path)) {
            owner.remove(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DirectoryStorage, Storage};

    #[tokio::test]
    async fn materialization_rolls_back_the_whole_depot_when_one_file_fails() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        storage
            .install_file(Box::new(Cursor::new(b"old")), "save/a.bin")
            .await
            .unwrap();
        // A file where the target needs a directory makes the second install
        // fail after a.bin has already been replaced.
        storage
            .install_file(Box::new(Cursor::new(b"blocker")), "zzblock")
            .await
            .unwrap();

        let old = BTreeMap::from([("save/a.bin".to_owned(), b"old".to_vec())]);
        let new = BTreeMap::from([
            ("save/a.bin".to_owned(), b"new".to_vec()),
            ("zzblock/child.bin".to_owned(), b"cannot install".to_vec()),
        ]);

        assert!(matches!(
            SaveVersioning::apply_files(&storage, &old, &new).await,
            Err(MaterializationFailure::RolledBack(_))
        ));
        assert_eq!(
            std::fs::read(directory.path().join("save/a.bin")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(directory.path().join("zzblock")).unwrap(),
            b"blocker"
        );
        assert!(!directory.path().join("zzblock/child.bin").exists());
    }
}
