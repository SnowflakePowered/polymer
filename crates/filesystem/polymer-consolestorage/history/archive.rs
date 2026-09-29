use std::collections::HashSet;

use gix::bstr::ByteSlice;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use jiff::Timestamp;
use uuid::Uuid;

use super::{
    AttachmentTips, DeletedSaveInfo, SaveAttachmentKind, SaveCommitId, SaveCommitInfo,
    SaveCommitOperation, SaveDepotId, SaveRefState, SaveTransition, SaveVersioning,
};
use crate::{Error, SaveRepo};

#[derive(Clone)]
struct DeletedRecord {
    info: DeletedSaveInfo,
    state: SaveRefState,
}

enum ArchiveRecord {
    Deleted(DeletedRecord),
    Restored { deletion_id: Uuid },
}

impl SaveVersioning {
    pub fn recently_deleted(&self) -> Result<Vec<DeletedSaveInfo>, Error> {
        let repo = self.inner.repository().to_thread_local();
        let archive_ref = self.archive_ref();
        let Some(reference) = repo
            .try_find_reference(&archive_ref)
            .map_err(|error| Error::git("find archive reference", error))?
        else {
            return Ok(Vec::new());
        };
        let mut platform = reference.log_iter();
        let Some(lines) = platform
            .rev()
            .map_err(|error| Error::io(&repo.git_dir().join("logs").join(&archive_ref), error))?
        else {
            return Ok(Vec::new());
        };
        let mut restored = HashSet::new();
        let mut deleted = Vec::new();
        for line in lines {
            let line = line.map_err(|error| Error::git("read archive reflog", error))?;
            let id = line.new_oid;
            if id.is_null() {
                continue;
            }
            let Ok(record) = Self::archive_record(&repo, id) else {
                continue;
            };
            match record {
                ArchiveRecord::Restored { deletion_id } => {
                    restored.insert(deletion_id);
                }
                ArchiveRecord::Deleted(info) if !restored.contains(&info.info.deletion_id) => {
                    deleted.push(info.info);
                }
                ArchiveRecord::Deleted(_) => {}
            }
        }
        Ok(deleted)
    }

    pub(crate) async fn restore_deleted<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        deletion_id: Uuid,
    ) -> Result<SaveCommitInfo, Error> {
        self.recover(owner).await?;
        let depot = {
            let repo = self.inner.repository().to_thread_local();
            self.find_deleted_record(&repo, deletion_id)?
                .ok_or_else(|| {
                    Error::HistoryConflict(format!("unknown or expired deletion {deletion_id}"))
                })?
                .info
                .depot
        };
        if owner.enumerate_saves().any(|save| save.id == depot) && self.tip(depot)?.is_none() {
            self.checkpoint(owner, depot, SaveCommitOperation::Manual)
                .await?;
        }
        let _guard = self.inner.operation.lock().await;
        let (record, conflict) = {
            let repo = self.inner.repository().to_thread_local();
            let record = self
                .find_deleted_record(&repo, deletion_id)?
                .ok_or_else(|| {
                    Error::HistoryConflict(format!("unknown or expired deletion {deletion_id}"))
                })?;
            let conflict = Self::ref_state_in(&repo, &self.refs(record.info.depot))?.is_some();
            (record, conflict)
        };
        if conflict {
            self.archive_and_remove_locked(owner, record.info.depot)
                .await?;
        }
        self.apply_transition_locked(
            owner,
            SaveTransition {
                depot: record.info.depot,
                before: None,
                after: Some(record.state),
            },
        )
        .await?;
        let repo = self.inner.repository().to_thread_local();
        self.pulse_archive(
            &repo,
            format!("Polymer archive restore\n\nPolymer-Deletion-Id: {deletion_id}\n"),
            std::iter::empty::<gix::ObjectId>(),
        )?;
        Self::commit_info(&repo, record.info.tip.0)
    }

    pub(crate) async fn archive_missing_saves<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        present: &HashSet<SaveDepotId>,
    ) -> Result<bool, Error> {
        self.recover(owner).await?;
        let tracked = self.tracked_depots()?;
        let _guard = self.inner.operation.lock().await;
        let mut changed = false;
        for depot in tracked {
            if !present.contains(&depot) {
                self.archive_and_remove_locked(owner, depot).await?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn parse_trailer<'a>(message: &'a str, key: &str) -> Option<&'a str> {
        message.lines().find_map(|line| line.strip_prefix(key))
    }

    fn archive_record(repo: &gix::Repository, id: gix::ObjectId) -> Result<ArchiveRecord, Error> {
        let commit = repo
            .find_commit(id)
            .map_err(|error| Error::git("find archive record", error))?;
        let message = commit.message_raw_sloppy().to_str_lossy();
        let deletion_id = Self::parse_trailer(&message, "Polymer-Deletion-Id: ")
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| {
                Error::HistoryConflict("archive record has no deletion id".to_owned())
            })?;
        if message.starts_with("Polymer archive restore") {
            return Ok(ArchiveRecord::Restored { deletion_id });
        }
        if !message.starts_with("Polymer archive deletion") {
            return Err(Error::HistoryConflict(
                "not a Polymer archive record".to_owned(),
            ));
        }
        let depot = Self::parse_trailer(&message, "Polymer-Depot: ")
            .and_then(|value| u64::from_str_radix(value, 16).ok())
            .map(SaveDepotId)
            .ok_or_else(|| Error::HistoryConflict("archive record has no depot".to_owned()))?;
        let mut parents = commit.parent_ids().map(|id| id.detach());
        let tip = parents
            .next()
            .ok_or_else(|| Error::HistoryConflict("archive record has no save tip".to_owned()))?;
        let screenshot = Self::parse_trailer(&message, "Polymer-Screenshot: ")
            .and_then(|value| gix::ObjectId::from_hex(value.as_bytes()).ok());
        let icon = Self::parse_trailer(&message, "Polymer-Icon: ")
            .and_then(|value| gix::ObjectId::from_hex(value.as_bytes()).ok());
        let note = Self::parse_trailer(&message, "Polymer-Note: ")
            .and_then(|value| gix::ObjectId::from_hex(value.as_bytes()).ok());
        let title = Self::parse_trailer(&message, "Polymer-Title: ")
            .and_then(|value| gix::ObjectId::from_hex(value.as_bytes()).ok());
        let deleted_at = Timestamp::from_second(
            commit
                .time()
                .map_err(|error| Error::git("read archive time", error))?
                .seconds,
        )?;
        Ok(ArchiveRecord::Deleted(DeletedRecord {
            info: DeletedSaveInfo {
                deletion_id,
                depot,
                deleted_at,
                tip: SaveCommitId(tip),
            },
            state: SaveRefState {
                tip,
                attachments: AttachmentTips {
                    screenshot,
                    icon,
                    note,
                    title,
                },
            },
        }))
    }

    fn ensure_archive_sentinel(&self, repo: &gix::Repository) -> Result<gix::ObjectId, Error> {
        let archive_ref = self.archive_ref();
        if let Some(id) = Self::tip_in(repo, &archive_ref)? {
            return Ok(id);
        }
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        repo.commit_as(
            signature,
            signature,
            archive_ref,
            "Polymer recently-deleted archive sentinel",
            repo.empty_tree().id,
            std::iter::empty::<gix::ObjectId>(),
        )
        .map(|id| id.detach())
        .map_err(|error| Error::git("create archive sentinel", error))
    }

    fn forced_ref_update(
        repo: &gix::Repository,
        name: &str,
        id: gix::ObjectId,
        message: &str,
    ) -> Result<(), Error> {
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        let name = gix::refs::FullName::try_from(name)
            .map_err(|error| Error::git("validate reference name", error))?;
        repo.edit_references_as(
            [RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: true,
                        message: message.into(),
                    },
                    expected: PreviousValue::Any,
                    new: Target::Object(id),
                },
                name,
                deref: false,
            }],
            Some(signature),
        )
        .map(|_| ())
        .map_err(|error| Error::git("update reference", error))
    }

    fn pulse_archive(
        &self,
        repo: &gix::Repository,
        message: String,
        parents: impl IntoIterator<Item = gix::ObjectId>,
    ) -> Result<gix::ObjectId, Error> {
        let sentinel = self.ensure_archive_sentinel(repo)?;
        let archive_ref = self.archive_ref();
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        let record = repo
            .new_commit_as(signature, signature, message, repo.empty_tree().id, parents)
            .map_err(|error| Error::git("write archive record", error))?
            .id;
        Self::forced_ref_update(repo, &archive_ref, record, "polymer: archive record")?;
        Self::forced_ref_update(repo, &archive_ref, sentinel, "polymer: archive sentinel")?;
        Ok(record)
    }

    pub(super) async fn archive_and_remove_locked<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        depot: SaveDepotId,
    ) -> Result<(), Error> {
        let Some(state) = ({
            let repo = self.inner.repository().to_thread_local();
            let Some(state) = Self::ref_state_in(&repo, &self.refs(depot))? else {
                return Ok(());
            };
            let deletion_id = Uuid::new_v4();
            let mut message = format!(
                "Polymer archive deletion\n\nPolymer-Deletion-Id: {deletion_id}\nPolymer-Platform: {}\nPolymer-Depot: {:016x}\nPolymer-Save-Tip: {}\n",
                self.inner.platform(),
                depot.0,
                state.tip
            );
            let mut parents = vec![state.tip];
            for (kind, id) in state.attachments.entries() {
                if let Some(id) = id {
                    let trailer = match kind {
                        SaveAttachmentKind::Screenshot => "Polymer-Screenshot",
                        SaveAttachmentKind::Icon => "Polymer-Icon",
                        SaveAttachmentKind::Note => "Polymer-Note",
                        SaveAttachmentKind::Title => "Polymer-Title",
                    };
                    message.push_str(&format!("{trailer}: {id}\n"));
                    parents.push(id);
                }
            }
            self.pulse_archive(&repo, message, parents)?;
            Some(state)
        }) else {
            return Ok(());
        };
        self.apply_transition_locked(
            owner,
            SaveTransition {
                depot,
                before: Some(state),
                after: None,
            },
        )
        .await
    }

    fn find_deleted_record(
        &self,
        repo: &gix::Repository,
        wanted: Uuid,
    ) -> Result<Option<DeletedRecord>, Error> {
        let Some(reference) = repo
            .try_find_reference(&self.archive_ref())
            .map_err(|error| Error::git("find archive", error))?
        else {
            return Ok(None);
        };
        let mut platform = reference.log_iter();
        let Some(lines) = platform.rev().map_err(|error| {
            Error::io(&repo.git_dir().join("logs").join(self.archive_ref()), error)
        })?
        else {
            return Ok(None);
        };
        for line in lines {
            let line = line.map_err(|error| Error::git("read archive reflog", error))?;
            let id = line.new_oid;
            match Self::archive_record(repo, id) {
                Ok(ArchiveRecord::Restored { deletion_id }) if deletion_id == wanted => {
                    return Ok(None);
                }
                Ok(ArchiveRecord::Deleted(record)) if record.info.deletion_id == wanted => {
                    return Ok(Some(record));
                }
                _ => {}
            }
        }
        Ok(None)
    }
}
