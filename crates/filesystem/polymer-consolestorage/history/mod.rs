//! Git-backed, per-save history for [`SaveRepo`](crate::SaveRepo) implementations.

mod archive;
mod attachments;
mod repository;
mod storage;
mod transaction;

use repository::copy_object_graph;
pub use storage::VersionedStorage;

use std::fmt;
use std::io::{Cursor, Read};
use std::str::FromStr;
use std::sync::Arc;

use jiff::Timestamp;
use tokio::io::AsyncRead;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::savegame::{SaveDepotId, SaveDescriptor};
use crate::{Error, SaveRepo};

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SaveCommitId(gix::ObjectId);

impl SaveCommitId {
    pub fn as_object_id(self) -> gix::ObjectId {
        self.0
    }
}

impl fmt::Debug for SaveCommitId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SaveCommitId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for SaveCommitId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        gix::ObjectId::from_hex(value.as_bytes())
            .map(Self)
            .map_err(|error| Error::git("parse object id", error))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveAttachmentKind {
    Note,
    Screenshot,
    Icon,
    Title,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentScope {
    Commit(SaveCommitId),
    History,
}

impl fmt::Display for AttachmentScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit(commit) => fmt::Display::fmt(commit, formatter),
            Self::History => formatter.write_str("history"),
        }
    }
}

impl SaveAttachmentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Screenshot => "screenshot",
            Self::Icon => "icon",
            Self::Title => "title",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveCommitOperation {
    InitialImport,
    Autosave,
    Manual,
    Restore { restored_from: SaveCommitId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveCommitInfo {
    pub id: SaveCommitId,
    pub parent: Option<SaveCommitId>,
    pub timestamp: Timestamp,
    pub operation: SaveCommitOperation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentInfo {
    pub object_id: SaveCommitId,
    pub byte_length: u64,
    pub media_type: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletedSaveInfo {
    pub deletion_id: Uuid,
    pub depot: SaveDepotId,
    pub deleted_at: Timestamp,
    pub tip: SaveCommitId,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AttachmentTips {
    screenshot: Option<gix::ObjectId>,
    icon: Option<gix::ObjectId>,
    note: Option<gix::ObjectId>,
    title: Option<gix::ObjectId>,
}

impl AttachmentTips {
    fn get(self, kind: SaveAttachmentKind) -> Option<gix::ObjectId> {
        match kind {
            SaveAttachmentKind::Screenshot => self.screenshot,
            SaveAttachmentKind::Icon => self.icon,
            SaveAttachmentKind::Note => self.note,
            SaveAttachmentKind::Title => self.title,
        }
    }

    fn entries(self) -> [(SaveAttachmentKind, Option<gix::ObjectId>); 4] {
        [
            (SaveAttachmentKind::Screenshot, self.screenshot),
            (SaveAttachmentKind::Icon, self.icon),
            (SaveAttachmentKind::Note, self.note),
            (SaveAttachmentKind::Title, self.title),
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SaveRefState {
    tip: gix::ObjectId,
    attachments: AttachmentTips,
}

impl SaveRefState {
    fn reachable_roots(self) -> impl Iterator<Item = gix::ObjectId> {
        [
            Some(self.tip),
            self.attachments.screenshot,
            self.attachments.icon,
            self.attachments.note,
            self.attachments.title,
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SaveTransition {
    depot: SaveDepotId,
    before: Option<SaveRefState>,
    after: Option<SaveRefState>,
}

#[derive(Clone, Debug)]
struct SaveRefs {
    head: String,
    screenshot: String,
    icon: String,
    note: String,
    title: String,
}

impl SaveRefs {
    fn attachment(&self, kind: SaveAttachmentKind) -> &str {
        match kind {
            SaveAttachmentKind::Screenshot => &self.screenshot,
            SaveAttachmentKind::Icon => &self.icon,
            SaveAttachmentKind::Note => &self.note,
            SaveAttachmentKind::Title => &self.title,
        }
    }

    fn entries(&self) -> [(&str, Option<SaveAttachmentKind>); 5] {
        [
            (&self.head, None),
            (&self.screenshot, Some(SaveAttachmentKind::Screenshot)),
            (&self.icon, Some(SaveAttachmentKind::Icon)),
            (&self.note, Some(SaveAttachmentKind::Note)),
            (&self.title, Some(SaveAttachmentKind::Title)),
        ]
    }
}

struct HistoryInner {
    profile: crate::GitVolumeStorage,
    operation: Mutex<()>,
}

impl HistoryInner {
    fn repository(&self) -> &gix::ThreadSafeRepository {
        self.profile.platform_repository().repository()
    }

    fn platform(&self) -> &str {
        self.profile.platform_repository().platform()
    }

    fn volume_id(&self) -> Uuid {
        self.profile.id()
    }
}

#[derive(Clone)]
pub struct SaveVersioning {
    inner: Arc<HistoryInner>,
}

impl fmt::Debug for SaveVersioning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SaveVersioning")
            .field("profile", &self.inner.profile)
            .finish_non_exhaustive()
    }
}

impl SaveVersioning {
    fn archive_ref(&self) -> String {
        format!(
            "refs/consoles/archive/v2/{}/recently-deleted",
            self.inner.volume_id().simple()
        )
    }

    pub fn platform(&self) -> &str {
        self.inner.platform()
    }

    pub fn volume_id(&self) -> Uuid {
        self.inner.volume_id()
    }

    fn commit_message(&self, depot: SaveDepotId, operation: &SaveCommitOperation) -> String {
        let operation = match operation {
            SaveCommitOperation::InitialImport => "initial-import".to_owned(),
            SaveCommitOperation::Autosave => "autosave".to_owned(),
            SaveCommitOperation::Manual => "manual".to_owned(),
            SaveCommitOperation::Restore { restored_from } => {
                format!("restore\nPolymer-Restored-From: {restored_from}")
            }
        };
        format!(
            "Polymer save {operation}\n\nPolymer-Schema: {SCHEMA_VERSION}\nPolymer-Platform: {}\nPolymer-Depot: {:016x}\n",
            self.inner.platform(),
            depot.0
        )
    }

    fn operation_from_message(message: &[u8]) -> SaveCommitOperation {
        let message = String::from_utf8_lossy(message);
        if let Some(value) = message
            .lines()
            .find_map(|line| line.strip_prefix("Polymer-Restored-From: "))
            .and_then(|value| value.parse().ok())
        {
            SaveCommitOperation::Restore {
                restored_from: value,
            }
        } else if message.starts_with("Polymer save initial-import") {
            SaveCommitOperation::InitialImport
        } else if message.starts_with("Polymer save autosave") {
            SaveCommitOperation::Autosave
        } else {
            SaveCommitOperation::Manual
        }
    }

    fn commit_info(repo: &gix::Repository, id: gix::ObjectId) -> Result<SaveCommitInfo, Error> {
        let commit = repo
            .find_commit(id)
            .map_err(|error| Error::git("find save commit", error))?;
        let parent = commit
            .parent_ids()
            .next()
            .map(|id| SaveCommitId(id.detach()));
        let timestamp = Timestamp::from_second(
            commit
                .time()
                .map_err(|error| Error::git("read commit time", error))?
                .seconds,
        )?;
        let operation = Self::operation_from_message(commit.message_raw_sloppy());
        Ok(SaveCommitInfo {
            id: SaveCommitId(id),
            parent,
            timestamp,
            operation,
        })
    }

    pub(crate) async fn checkpoint<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        depot: SaveDepotId,
        requested: SaveCommitOperation,
    ) -> Result<Option<SaveCommitInfo>, Error> {
        self.recover(owner).await?;
        let exported = owner.export_save(&depot)?;
        let guard = self.inner.operation.lock().await;
        let repo = self.inner.repository().to_thread_local();
        let reference = self.refs(depot).head;
        let parent = Self::tip_in(&repo, &reference)?;
        let tree = self.write_snapshot(&repo, &exported.blobs)?;
        if let Some(parent) = parent {
            let current_tree = Self::commit_tree(&repo, parent)?;
            if current_tree == tree {
                self.inner
                    .profile
                    .commit_pending("Polymer profile autosave", None)?;
                drop(guard);
                owner.save_icon(depot).await?;
                return Ok(None);
            }
        }
        let operation = if parent.is_none() {
            SaveCommitOperation::InitialImport
        } else {
            requested
        };
        let message = self.commit_message(depot, &operation);
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        let parents = parent.into_iter().collect::<Vec<_>>();
        let id = repo
            .commit_as(signature, signature, reference, message, tree, parents)
            .map_err(|error| Error::git("commit save snapshot", error))?
            .detach();
        self.inner.profile.set_save_tip(depot.0, Some(id));
        self.inner
            .profile
            .commit_pending("Polymer profile autosave", None)?;
        let info = Self::commit_info(&repo, id)?;
        drop(guard);
        owner.save_icon(depot).await?;
        Ok(Some(info))
    }

    pub(crate) fn checkpoint_profile(&self) -> Result<bool, Error> {
        self.inner
            .profile
            .commit_pending("Polymer profile autosave", None)
            .map(|commit| commit.is_some())
    }

    fn history(&self, depot: SaveDepotId) -> Result<Vec<SaveCommitInfo>, Error> {
        let repo = self.inner.repository().to_thread_local();
        let mut current = Self::tip_in(&repo, &self.refs(depot).head)?;
        let mut history = Vec::new();
        while let Some(id) = current {
            let info = Self::commit_info(&repo, id)?;
            current = info.parent.map(|id| id.0);
            history.push(info);
        }
        Ok(history)
    }

    fn ensure_member(&self, depot: SaveDepotId, wanted: SaveCommitId) -> Result<(), Error> {
        if self
            .history(depot)?
            .iter()
            .any(|commit| commit.id == wanted)
        {
            Ok(())
        } else {
            Err(Error::UnknownSaveCommit(wanted.to_string()))
        }
    }

    async fn restore<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        depot: SaveDepotId,
        wanted: SaveCommitId,
    ) -> Result<SaveCommitInfo, Error> {
        self.checkpoint(owner, depot, SaveCommitOperation::Manual)
            .await?;
        let guard = self.inner.operation.lock().await;
        self.ensure_member(depot, wanted)?;
        let refs = self.refs(depot);
        let operation = SaveCommitOperation::Restore {
            restored_from: wanted,
        };
        let (before, after) = {
            let repo = self.inner.repository().to_thread_local();
            let before = Self::ref_state_in(&repo, &refs)?
                .ok_or_else(|| Error::UnknownSaveCommit(wanted.to_string()))?;
            let signature = Self::signature();
            let mut signature_time = gix::date::parse::TimeBuf::default();
            let signature = signature.to_ref(&mut signature_time);
            let id = repo
                .new_commit_as(
                    signature,
                    signature,
                    self.commit_message(depot, &operation),
                    Self::commit_tree(&repo, wanted.0)?,
                    [before.tip],
                )
                .map_err(|error| Error::git("write restored save commit", error))?
                .id;
            let after = SaveRefState {
                tip: id,
                attachments: before.attachments,
            };
            (before, after)
        };
        self.apply_transition_locked(
            owner,
            SaveTransition {
                depot,
                before: Some(before),
                after: Some(after),
            },
        )
        .await?;
        let repo = self.inner.repository().to_thread_local();
        let info = Self::commit_info(&repo, after.tip)?;
        drop(guard);
        owner.save_icon(depot).await?;
        Ok(info)
    }
}

pub struct Save<'repo> {
    repo: &'repo dyn SaveRepo,
    id: SaveDepotId,
}

impl dyn SaveRepo + '_ {
    pub fn save(&self, id: SaveDepotId) -> Save<'_> {
        Save::new(self, id)
    }
}

impl<'repo> Save<'repo> {
    pub fn new(repo: &'repo dyn SaveRepo, id: SaveDepotId) -> Self {
        Self { repo, id }
    }

    pub fn id(&self) -> SaveDepotId {
        self.id
    }

    pub fn descriptor(&self) -> Result<SaveDescriptor, Error> {
        self.repo
            .enumerate_saves()
            .find(|descriptor| descriptor.id == self.id)
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", self.id.0)))
    }

    pub fn history(&self) -> Result<Vec<SaveCommitInfo>, Error> {
        self.repo.versioning().history(self.id)
    }

    pub fn commit(&self, id: SaveCommitId) -> Result<SaveCommit<'repo>, Error> {
        self.repo.versioning().ensure_member(self.id, id)?;
        Ok(SaveCommit {
            repo: self.repo,
            depot: self.id,
            id,
        })
    }

    pub async fn checkpoint(&self) -> Result<Option<SaveCommitInfo>, Error> {
        self.repo
            .versioning()
            .checkpoint(self.repo, self.id, SaveCommitOperation::Manual)
            .await
    }

    pub async fn restore(&self, id: SaveCommitId) -> Result<SaveCommitInfo, Error> {
        self.repo.versioning().restore(self.repo, self.id, id).await
    }

    pub async fn copy_to(&self, destination: &dyn SaveRepo) -> Result<SaveCommitInfo, Error> {
        copy_to(self.repo, self.id, destination).await
    }

    pub async fn delete(&self) -> Result<(), Error> {
        delete_save(self.repo, self.id).await
    }
}

pub struct SaveCommit<'repo> {
    repo: &'repo dyn SaveRepo,
    depot: SaveDepotId,
    id: SaveCommitId,
}

impl SaveCommit<'_> {
    pub fn id(&self) -> SaveCommitId {
        self.id
    }

    pub fn info(&self) -> Result<SaveCommitInfo, Error> {
        self.repo.versioning().ensure_member(self.depot, self.id)?;
        SaveVersioning::commit_info(
            &self.repo.versioning().inner.repository().to_thread_local(),
            self.id.0,
        )
    }

    pub fn open_attachment(
        &self,
        kind: SaveAttachmentKind,
    ) -> Result<Option<Box<dyn Read + Send>>, Error> {
        self.repo
            .versioning()
            .attachment(self.depot, AttachmentScope::Commit(self.id), kind)
            .map(|bytes| bytes.map(|bytes| Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>))
    }

    pub async fn attach(
        &self,
        kind: SaveAttachmentKind,
        source: Box<dyn AsyncRead + Send + Unpin>,
    ) -> Result<AttachmentInfo, Error> {
        self.repo
            .versioning()
            .attach(self.depot, AttachmentScope::Commit(self.id), kind, source)
            .await
    }

    pub async fn remove_attachment(&self, kind: SaveAttachmentKind) -> Result<bool, Error> {
        self.repo
            .versioning()
            .remove_attachment(self.depot, AttachmentScope::Commit(self.id), kind)
            .await
    }

    pub fn note(&self) -> Result<Option<String>, Error> {
        let Some(bytes) = self.repo.versioning().attachment(
            self.depot,
            AttachmentScope::Commit(self.id),
            SaveAttachmentKind::Note,
        )?
        else {
            return Ok(None);
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|error| Error::HistoryConflict(format!("commit note is not UTF-8: {error}")))
    }

    pub async fn set_note(&self, note: impl Into<String>) -> Result<AttachmentInfo, Error> {
        self.attach(
            SaveAttachmentKind::Note,
            Box::new(Cursor::new(note.into().into_bytes())),
        )
        .await
    }

    pub async fn remove_note(&self) -> Result<bool, Error> {
        self.remove_attachment(SaveAttachmentKind::Note).await
    }
}

// Copy/delete/archive are implemented below so their public API is available
// while their plumbing remains private to this module.
async fn copy_to(
    source: &dyn SaveRepo,
    depot: SaveDepotId,
    destination: &dyn SaveRepo,
) -> Result<SaveCommitInfo, Error> {
    source
        .versioning()
        .checkpoint(source, depot, SaveCommitOperation::Manual)
        .await?;
    if source.versioning().platform() != destination.versioning().platform() {
        return Err(Error::Unsupported(format!(
            "cannot copy a {} save to a {} SaveRepo",
            source.versioning().platform(),
            destination.versioning().platform()
        )));
    }
    if destination.enumerate_saves().any(|save| save.id == depot) {
        destination
            .versioning()
            .checkpoint(destination, depot, SaveCommitOperation::Manual)
            .await?;
    }
    copy_save_locked(source, depot, destination).await
}

async fn delete_save(owner: &dyn SaveRepo, depot: SaveDepotId) -> Result<(), Error> {
    owner
        .versioning()
        .checkpoint(owner, depot, SaveCommitOperation::Manual)
        .await?;
    delete_save_locked(owner, depot).await
}

async fn copy_save_locked(
    source: &dyn SaveRepo,
    depot: SaveDepotId,
    destination: &dyn SaveRepo,
) -> Result<SaveCommitInfo, Error> {
    let source_core = source.versioning();
    let destination_core = destination.versioning();
    if Arc::ptr_eq(&source_core.inner, &destination_core.inner) {
        let tip = source_core
            .tip(depot)?
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", depot.0)))?;
        return SaveVersioning::commit_info(&source_core.inner.repository().to_thread_local(), tip);
    }

    let _guard = destination_core.inner.operation.lock().await;
    destination_core.recover_locked(destination).await?;
    let (source_state, conflict) = {
        let source_repo = source_core.inner.repository().to_thread_local();
        let source_state = SaveVersioning::ref_state_in(&source_repo, &source_core.refs(depot))?
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", depot.0)))?;
        let destination_repo = destination_core.inner.repository().to_thread_local();
        for root in source_state.reachable_roots() {
            copy_object_graph(&source_repo, &destination_repo, root)?;
        }
        let conflict =
            SaveVersioning::ref_state_in(&destination_repo, &destination_core.refs(depot))?
                .is_some();
        (source_state, conflict)
    };
    if conflict {
        destination_core
            .archive_and_remove_locked(destination, depot)
            .await?;
    }
    destination_core
        .apply_transition_locked(
            destination,
            SaveTransition {
                depot,
                before: None,
                after: Some(source_state),
            },
        )
        .await?;
    let destination_repo = destination_core.inner.repository().to_thread_local();
    SaveVersioning::commit_info(&destination_repo, source_state.tip)
}

async fn delete_save_locked(owner: &dyn SaveRepo, depot: SaveDepotId) -> Result<(), Error> {
    let core = owner.versioning();
    let _guard = core.inner.operation.lock().await;
    core.archive_and_remove_locked(owner, depot).await
}
