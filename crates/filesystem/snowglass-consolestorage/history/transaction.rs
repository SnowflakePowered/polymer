use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    AttachmentTips, JOURNAL_FILE, SaveDepotId, SaveRefState, SaveTransition, SaveVersioning,
};
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
    fn journal_path(&self) -> PathBuf {
        self.inner.repository.git_dir().join(JOURNAL_FILE)
    }

    fn parse_journal_id(value: Option<String>) -> Result<Option<gix::ObjectId>, Error> {
        value
            .map(|value| {
                gix::ObjectId::from_hex(value.as_bytes())
                    .map_err(|error| Error::git("parse transaction object id", error))
            })
            .transpose()
    }

    fn read_journal(&self) -> Result<Option<MaterializationJournal>, Error> {
        let path = self.journal_path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| Error::git("read history transaction", error)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::io(&path, error)),
        }
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
        let path = self.journal_path();
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&journal)
            .map_err(|error| Error::git("encode history transaction", error))?;
        std::fs::write(&temporary, bytes).map_err(|error| Error::io(&temporary, error))?;
        std::fs::rename(&temporary, &path).map_err(|error| Error::io(&path, error))
    }

    fn clear_journal(&self) -> Result<(), Error> {
        let path = self.journal_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::io(&path, error)),
        }
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
            let repo = self.inner.repository.to_thread_local();
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
        let repo = self.inner.repository.to_thread_local();
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
