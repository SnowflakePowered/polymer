//! Git-backed, per-save history for [`SaveRepo`](crate::SaveRepo) implementations.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use gix::objs::Write;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::savegame::{SaveBlob, SaveDepotId, SaveDescriptor};
use crate::{
    ArtifactRegistry, ArtifactSummary, BoxFuture, ChangeSignal, DirectoryStorage, Error,
    InstallResult, Metadata, MountableStorage, ReadDir, SaveRepo, Storage, StorageFile, VolumeInfo,
    VolumeStorage,
};

const SCHEMA_VERSION: u32 = 1;
const ARCHIVE_REF: &str = "refs/snowglass/archive/v1/recently-deleted";
const MARKER_FILE: &str = "snowglass.json";
const JOURNAL_FILE: &str = "snowglass-transaction.json";
const VOLUME_REF: &str = "refs/snowglass/volume/v1";

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
}

impl SaveAttachmentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Screenshot => "screenshot",
            Self::Icon => "icon",
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

#[derive(Serialize, Deserialize)]
struct Marker {
    schema: u32,
    platform: String,
    volume_id: Uuid,
}

#[derive(Debug)]
struct EmptyArtifactRegistry;

static EMPTY_ARTIFACTS: EmptyArtifactRegistry = EmptyArtifactRegistry;

impl ArtifactRegistry for EmptyArtifactRegistry {
    fn record<'a>(
        &'a self,
        _artifact: &'a crate::installable::artifacts::Artifact,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async { Ok(()) })
    }

    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn artifact<'a>(
        &'a self,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<Option<crate::installable::artifacts::Artifact>, Error>> {
        Box::pin(async { Ok(None) })
    }

    fn contains<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<bool, Error>> {
        Box::pin(async { Ok(false) })
    }

    fn forget<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Serialize, Deserialize)]
struct MaterializationJournal {
    depot: u64,
    old: Option<String>,
    target: Option<String>,
    screenshot: Option<String>,
    icon: Option<String>,
    note: Option<String>,
}

struct HistoryInner {
    root: PathBuf,
    platform: String,
    volume_id: Uuid,
    repository: gix::ThreadSafeRepository,
    operation: Mutex<()>,
}

/// A directory-backed volume with a colocated Git history database.
#[derive(Clone, Debug)]
pub struct VersionedStorage {
    storage: Arc<DirectoryStorage>,
    history: SaveVersioning,
}

impl VersionedStorage {
    pub fn new(storage: DirectoryStorage, platform: &str, volume_id: Uuid) -> Result<Self, Error> {
        let history = SaveVersioning::open(storage.root(), platform, volume_id)?;
        Ok(Self {
            storage: Arc::new(storage),
            history,
        })
    }

    pub fn versioning(&self) -> &SaveVersioning {
        &self.history
    }

    pub(crate) fn directory(&self) -> Arc<DirectoryStorage> {
        self.storage.clone()
    }
}

impl Storage for VersionedStorage {
    fn exists(&self, path: &str) -> bool {
        self.storage.exists(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.storage.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        self.storage.read_dir(path, recursive)
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        self.storage.create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        self.storage.create(path)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        self.storage.open(path, write)
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.storage.remove(path)
    }

    fn watch(&self, paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        self.storage.watch(paths)
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        self.storage.install_file(source, dest)
    }
}

impl MountableStorage for VersionedStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.storage.rename(from, to)
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.storage.truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.storage.sync(path)
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        self.storage.finish(path)
    }
}

impl VolumeStorage for VersionedStorage {
    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        Box::pin(async move { Ok(self.history.volume_info()?.and_then(|info| info.label)) })
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(self.history.set_volume_label(label))
    }

    fn artifacts(&self) -> &dyn ArtifactRegistry {
        &EMPTY_ARTIFACTS
    }

    fn volumeinfo(&self) -> BoxFuture<'_, Result<Option<VolumeInfo>, Error>> {
        Box::pin(async move { self.history.volume_info() })
    }

    fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<VolumeInfo, Error>> {
        Box::pin(self.history.brand_volume(id, platform, label))
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
            .field("root", &self.inner.root)
            .field("platform", &self.inner.platform)
            .field("volume_id", &self.inner.volume_id)
            .finish_non_exhaustive()
    }
}

impl SaveVersioning {
    fn open(root: &Path, platform: &str, volume_id: Uuid) -> Result<Self, Error> {
        let root = root.to_owned();
        let git_dir = root.join(".git");
        let repository = if git_dir.exists() {
            gix::open(&root)
                .map_err(|error| Error::git("open repository", error))?
                .into_sync()
        } else {
            let options = gix::create::Options {
                destination_must_be_empty: Some(false),
                ..Default::default()
            };
            gix::ThreadSafeRepository::init(&root, gix::create::Kind::WithWorktree, options)
                .map_err(|error| Error::git("initialize repository", error))?
        };

        let marker_path = repository.git_dir().join(MARKER_FILE);
        Self::configure_git_repository(&repository)?;
        let expected = Marker {
            schema: SCHEMA_VERSION,
            platform: platform.to_owned(),
            volume_id,
        };
        if marker_path.exists() {
            let bytes =
                std::fs::read(&marker_path).map_err(|error| Error::io(&marker_path, error))?;
            let marker: Marker = serde_json::from_slice(&bytes)
                .map_err(|error| Error::git("read Snowglass repository marker", error))?;
            if marker.schema != expected.schema
                || marker.platform != expected.platform
                || marker.volume_id != expected.volume_id
            {
                return Err(Error::HistoryConflict(format!(
                    "{} belongs to schema {} platform {} volume {}",
                    git_dir.display(),
                    marker.schema,
                    marker.platform,
                    marker.volume_id
                )));
            }
        } else {
            let bytes = serde_json::to_vec_pretty(&expected)
                .map_err(|error| Error::git("encode repository marker", error))?;
            std::fs::write(&marker_path, bytes).map_err(|error| Error::io(&marker_path, error))?;
            let exclude = repository.git_dir().join("info").join("exclude");
            let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
            if !existing.contains("# snowglass canonical storage") {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&exclude)
                    .map_err(|error| Error::io(&exclude, error))?;
                std::io::Write::write_all(&mut file, b"\n# snowglass canonical storage\n*\n")
                    .map_err(|error| Error::io(&exclude, error))?;
            }
        }

        Ok(Self {
            inner: Arc::new(HistoryInner {
                root,
                platform: platform.to_owned(),
                volume_id,
                repository,
                operation: Mutex::new(()),
            }),
        })
    }

    pub fn platform(&self) -> &str {
        &self.inner.platform
    }

    pub fn volume_id(&self) -> Uuid {
        self.inner.volume_id
    }

    fn configure_git_repository(repository: &gix::ThreadSafeRepository) -> Result<(), Error> {
        let config_path = repository.git_dir().join("config");
        let existing = std::fs::read_to_string(&config_path)
            .map_err(|error| Error::io(&config_path, error))?;
        if existing.contains("# snowglass history policy v1") {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&config_path)
            .map_err(|error| Error::io(&config_path, error))?;
        std::io::Write::write_all(
            &mut file,
            b"\n# snowglass history policy v1\n[core]\n\tlogAllRefUpdates = always\n[gc \"refs/snowglass/archive/v1/recently-deleted\"]\n\treflogExpireUnreachable = 30 days\n",
        )
        .map_err(|error| Error::io(&config_path, error))
    }

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

    fn write_journal(
        &self,
        depot: SaveDepotId,
        old: Option<gix::ObjectId>,
        target: Option<gix::ObjectId>,
        screenshot: Option<gix::ObjectId>,
        icon: Option<gix::ObjectId>,
        note: Option<gix::ObjectId>,
    ) -> Result<(), Error> {
        let journal = MaterializationJournal {
            depot: depot.0,
            old: old.map(|id| id.to_string()),
            target: target.map(|id| id.to_string()),
            screenshot: screenshot.map(|id| id.to_string()),
            icon: icon.map(|id| id.to_string()),
            note: note.map(|id| id.to_string()),
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

    async fn recover_locked<R: SaveRepo + ?Sized>(&self, owner: &R) -> Result<(), Error> {
        let Some(journal) = self.read_journal()? else {
            return Ok(());
        };
        let depot = SaveDepotId(journal.depot);
        let old = Self::parse_journal_id(journal.old)?;
        let target = Self::parse_journal_id(journal.target)?;
        let screenshot = Self::parse_journal_id(journal.screenshot)?;
        let icon = Self::parse_journal_id(journal.icon)?;
        let note = Self::parse_journal_id(journal.note)?;
        let (old_files, new_files) = {
            let repo = self.inner.repository.to_thread_local();
            (
                old.map(|id| Self::commit_files(&repo, id))
                    .transpose()?
                    .unwrap_or_default(),
                target
                    .map(|id| Self::commit_files(&repo, id))
                    .transpose()?
                    .unwrap_or_default(),
            )
        };
        Self::apply_files(owner, &old_files, &new_files).await?;
        let repo = self.inner.repository.to_thread_local();
        match target {
            Some(tip) => self.install_save_refs(&repo, depot, tip, screenshot, icon, note)?,
            None => self.delete_refs(&repo, depot)?,
        }
        self.clear_journal()
    }

    async fn materialize_locked<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        depot: SaveDepotId,
        old: Option<gix::ObjectId>,
        target: Option<gix::ObjectId>,
        screenshot: Option<gix::ObjectId>,
        icon: Option<gix::ObjectId>,
        note: Option<gix::ObjectId>,
    ) -> Result<(), Error> {
        self.write_journal(depot, old, target, screenshot, icon, note)?;
        self.recover_locked(owner).await
    }

    async fn recover<R: SaveRepo + ?Sized>(&self, owner: &R) -> Result<(), Error> {
        let _guard = self.inner.operation.lock().await;
        self.recover_locked(owner).await
    }

    fn save_ref(&self, depot: SaveDepotId) -> String {
        format!(
            "refs/snowglass/saves/v1/{}/{:016x}",
            self.inner.platform, depot.0
        )
    }

    fn notes_ref(&self, depot: SaveDepotId, kind: SaveAttachmentKind) -> String {
        format!(
            "refs/notes/snowglass/v1/{}/{:016x}/{}",
            self.inner.platform,
            depot.0,
            kind.as_str()
        )
    }

    fn tracked_depots(&self) -> Result<Vec<SaveDepotId>, Error> {
        let repo = self.inner.repository.to_thread_local();
        let prefix = format!("refs/snowglass/saves/v1/{}/", self.inner.platform);
        let mut depots = Vec::new();
        let references = repo
            .references()
            .map_err(|error| Error::git("list save references", error))?;
        for reference in references
            .all()
            .map_err(|error| Error::git("list save references", error))?
        {
            let reference = reference.map_err(|error| Error::git("read save reference", error))?;
            let name = reference.name().as_bstr().to_str_lossy();
            let Some(value) = name.strip_prefix(&prefix) else {
                continue;
            };
            if let Ok(value) = u64::from_str_radix(value, 16) {
                depots.push(SaveDepotId(value));
            }
        }
        Ok(depots)
    }

    fn tip_in(repo: &gix::Repository, name: &str) -> Result<Option<gix::ObjectId>, Error> {
        repo.try_find_reference(name)
            .map_err(|error| Error::git("find reference", error))
            .map(|reference| {
                reference.and_then(|reference| reference.try_id().map(|id| id.detach()))
            })
    }

    fn tip(&self, depot: SaveDepotId) -> Result<Option<gix::ObjectId>, Error> {
        Self::tip_in(
            &self.inner.repository.to_thread_local(),
            &self.save_ref(depot),
        )
    }

    fn volume_info(&self) -> Result<Option<VolumeInfo>, Error> {
        let repo = self.inner.repository.to_thread_local();
        let Some(tip) = Self::tip_in(&repo, VOLUME_REF)? else {
            return Ok(None);
        };
        let tree = Self::commit_tree(&repo, tip)?;
        let mut values = Self::tree_files(&repo, tree)?;
        let id = Self::volume_value(&mut values, "id")?
            .parse()
            .map_err(|error| Error::git("parse volume id", error))?;
        let platform = Self::volume_value(&mut values, "platform")?;
        let created = Self::volume_value(&mut values, "created")?
            .parse::<i64>()
            .map_err(|error| Error::git("parse volume creation time", error))?;
        let label = values
            .remove("label")
            .map(String::from_utf8)
            .transpose()
            .map_err(|error| Error::git("decode volume label", error))?;
        let info = VolumeInfo {
            id,
            platform,
            label,
            created: Timestamp::from_second(created)?,
        };
        if info.id != self.inner.volume_id || info.platform != self.inner.platform {
            return Err(Error::HistoryConflict(format!(
                "volume manifest identifies {} {}, expected {} {}",
                info.platform, info.id, self.inner.platform, self.inner.volume_id
            )));
        }
        Ok(Some(info))
    }

    fn volume_value(values: &mut BTreeMap<String, Vec<u8>>, key: &str) -> Result<String, Error> {
        let value = values
            .remove(key)
            .ok_or_else(|| Error::HistoryConflict(format!("volume commit has no `{key}` value")))?;
        String::from_utf8(value).map_err(|error| Error::git("decode volume value", error))
    }

    fn write_volume_manifest(
        &self,
        repo: &gix::Repository,
        info: &VolumeInfo,
        parent: Option<gix::ObjectId>,
        message: &str,
    ) -> Result<(), Error> {
        let mut editor = repo
            .empty_tree()
            .edit()
            .map_err(|error| Error::git("edit volume manifest tree", error))?;
        let id = info.id.to_string();
        let created = info.created.as_second().to_string();
        let mut values = vec![
            ("id", id.as_str()),
            ("platform", info.platform.as_str()),
            ("created", created.as_str()),
        ];
        if let Some(label) = info.label.as_deref() {
            values.push(("label", label));
        }
        for (key, value) in values {
            let blob = repo
                .write_blob(value.as_bytes())
                .map_err(|error| Error::git("write volume value", error))?
                .detach();
            editor
                .upsert(key, gix::objs::tree::EntryKind::Blob, blob)
                .map_err(|error| Error::git("add volume value", error))?;
        }
        let tree = editor
            .write()
            .map_err(|error| Error::git("write volume manifest tree", error))?
            .detach();
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        repo.commit_as(signature, signature, VOLUME_REF, message, tree, parent)
            .map(|_| ())
            .map_err(|error| Error::git("commit volume manifest", error))
    }

    async fn brand_volume(
        &self,
        id: Uuid,
        platform: &str,
        label: Option<&str>,
    ) -> Result<VolumeInfo, Error> {
        let _guard = self.inner.operation.lock().await;
        if let Some(info) = self.volume_info()? {
            return Ok(info);
        }
        if id != self.inner.volume_id || platform != self.inner.platform {
            return Err(Error::HistoryConflict(format!(
                "cannot brand {} {} as {platform} {id}",
                self.inner.platform, self.inner.volume_id
            )));
        }
        let info = VolumeInfo {
            id,
            platform: platform.to_owned(),
            label: label.map(str::to_owned),
            created: Timestamp::from_second(Timestamp::now().as_second())?,
        };
        let repo = self.inner.repository.to_thread_local();
        self.write_volume_manifest(&repo, &info, None, "Snowglass volume created")?;
        Ok(info)
    }

    async fn set_volume_label(&self, label: Option<&str>) -> Result<(), Error> {
        let _guard = self.inner.operation.lock().await;
        let mut info = self.volume_info()?.ok_or(Error::UnclaimedNand)?;
        if info.label.as_deref() == label {
            return Ok(());
        }
        info.label = label.map(str::to_owned);
        let repo = self.inner.repository.to_thread_local();
        let parent = Self::tip_in(&repo, VOLUME_REF)?;
        self.write_volume_manifest(&repo, &info, parent, "Snowglass volume label changed")
    }

    fn validate_blob_path(path: &Path) -> Result<String, Error> {
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(Error::InvalidPath(path.display().to_string()));
        }
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => {
                    let value = value
                        .to_str()
                        .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?;
                    if value.is_empty() || value == ".git" || value.contains(['/', '\\']) {
                        return Err(Error::InvalidPath(path.display().to_string()));
                    }
                    parts.push(value);
                }
                _ => return Err(Error::InvalidPath(path.display().to_string())),
            }
        }
        Ok(parts.join("/"))
    }

    fn write_snapshot(
        &self,
        repo: &gix::Repository,
        blobs: &[SaveBlob],
    ) -> Result<gix::ObjectId, Error> {
        let mut paths = BTreeSet::new();
        let empty = repo.empty_tree();
        let mut editor = empty
            .edit()
            .map_err(|error| Error::git("start tree editor", error))?;
        for blob in blobs {
            let path = Self::validate_blob_path(&blob.target_path)?;
            if !paths.insert(path.clone()) {
                return Err(Error::HistoryConflict(format!(
                    "duplicate save path `{path}`"
                )));
            }
            let id = repo
                .write_blob(&blob.bytes)
                .map_err(|error| Error::git("write save blob", error))?
                .detach();
            editor
                .upsert(path, gix::objs::tree::EntryKind::Blob, id)
                .map_err(|error| Error::git("edit save tree", error))?;
        }
        editor
            .write()
            .map(|id| id.detach())
            .map_err(|error| Error::git("write save tree", error))
    }

    fn signature() -> gix::actor::Signature {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let time = format!("{seconds} +0000");
        gix::actor::SignatureRef {
            name: b"Snowglass".as_bstr(),
            email: b"history@snowglass.invalid".as_bstr(),
            time: &time,
        }
        .to_owned()
        .expect("Snowglass generates a valid Git timestamp")
    }

    fn commit_message(&self, depot: SaveDepotId, operation: &SaveCommitOperation) -> String {
        let operation = match operation {
            SaveCommitOperation::InitialImport => "initial-import".to_owned(),
            SaveCommitOperation::Autosave => "autosave".to_owned(),
            SaveCommitOperation::Manual => "manual".to_owned(),
            SaveCommitOperation::Restore { restored_from } => {
                format!("restore\nSnowglass-Restored-From: {restored_from}")
            }
        };
        format!(
            "Snowglass save {operation}\n\nSnowglass-Schema: {SCHEMA_VERSION}\nSnowglass-Platform: {}\nSnowglass-Depot: {:016x}\n",
            self.inner.platform, depot.0
        )
    }

    fn operation_from_message(message: &[u8]) -> SaveCommitOperation {
        let message = String::from_utf8_lossy(message);
        if let Some(value) = message
            .lines()
            .find_map(|line| line.strip_prefix("Snowglass-Restored-From: "))
            .and_then(|value| value.parse().ok())
        {
            SaveCommitOperation::Restore {
                restored_from: value,
            }
        } else if message.starts_with("Snowglass save initial-import") {
            SaveCommitOperation::InitialImport
        } else if message.starts_with("Snowglass save autosave") {
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
        let _guard = self.inner.operation.lock().await;
        let repo = self.inner.repository.to_thread_local();
        let reference = self.save_ref(depot);
        let parent = Self::tip_in(&repo, &reference)?;
        let tree = self.write_snapshot(&repo, &exported.blobs)?;
        if let Some(parent) = parent {
            let current_tree = Self::commit_tree(&repo, parent)?;
            if current_tree == tree {
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
        Self::commit_info(&repo, id).map(Some)
    }

    fn history(&self, depot: SaveDepotId) -> Result<Vec<SaveCommitInfo>, Error> {
        let repo = self.inner.repository.to_thread_local();
        let mut current = Self::tip_in(&repo, &self.save_ref(depot))?;
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

    fn tree_files(
        repo: &gix::Repository,
        tree: gix::ObjectId,
    ) -> Result<BTreeMap<String, Vec<u8>>, Error> {
        let tree = repo
            .find_tree(tree)
            .map_err(|error| Error::git("find save tree", error))?;
        let entries = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|error| Error::git("traverse save tree", error))?;
        let mut files = BTreeMap::new();
        for entry in entries {
            if entry.mode.kind() != gix::objs::tree::EntryKind::Blob {
                continue;
            }
            let path = entry
                .filepath
                .to_str()
                .map_err(|error| Error::git("decode save path", error))?
                .to_owned();
            let blob = repo
                .find_blob(entry.oid)
                .map_err(|error| Error::git("read save blob", error))?;
            files.insert(path, blob.data.clone());
        }
        Ok(files)
    }

    fn commit_files(
        repo: &gix::Repository,
        commit: gix::ObjectId,
    ) -> Result<BTreeMap<String, Vec<u8>>, Error> {
        let tree = Self::commit_tree(repo, commit)?;
        Self::tree_files(repo, tree)
    }

    fn commit_tree(repo: &gix::Repository, commit: gix::ObjectId) -> Result<gix::ObjectId, Error> {
        repo.find_commit(commit)
            .map_err(|error| Error::git("find commit", error))?
            .tree_id()
            .map(|id| id.detach())
            .map_err(|error| Error::git("read commit tree", error))
    }

    async fn apply_files<R: SaveRepo + ?Sized>(
        owner: &R,
        old: &BTreeMap<String, Vec<u8>>,
        new: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        for path in old.keys().filter(|path| !new.contains_key(*path)) {
            owner.remove(path)?;
        }
        for (path, bytes) in new {
            owner
                .install_file(Box::new(Cursor::new(bytes.clone())), path)
                .await?;
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

    async fn restore<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        depot: SaveDepotId,
        wanted: SaveCommitId,
    ) -> Result<SaveCommitInfo, Error> {
        self.checkpoint(owner, depot, SaveCommitOperation::Manual)
            .await?;
        let _guard = self.inner.operation.lock().await;
        self.ensure_member(depot, wanted)?;
        let reference = self.save_ref(depot);
        let operation = SaveCommitOperation::Restore {
            restored_from: wanted,
        };
        let (parent, id, screenshot, icon, note) = {
            let repo = self.inner.repository.to_thread_local();
            let parent = Self::tip_in(&repo, &reference)?
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
                    [parent],
                )
                .map_err(|error| Error::git("write restored save commit", error))?
                .id;
            (
                parent,
                id,
                Self::tip_in(
                    &repo,
                    &self.notes_ref(depot, SaveAttachmentKind::Screenshot),
                )?,
                Self::tip_in(&repo, &self.notes_ref(depot, SaveAttachmentKind::Icon))?,
                Self::tip_in(&repo, &self.notes_ref(depot, SaveAttachmentKind::Note))?,
            )
        };
        self.materialize_locked(owner, depot, Some(parent), Some(id), screenshot, icon, note)
            .await?;
        let repo = self.inner.repository.to_thread_local();
        Self::commit_info(&repo, id)
    }

    fn note_path(commit: SaveCommitId) -> String {
        let id = commit.to_string();
        format!("{}/{}", &id[..2], &id[2..])
    }

    fn attachment(
        &self,
        depot: SaveDepotId,
        commit: SaveCommitId,
        kind: SaveAttachmentKind,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.ensure_member(depot, commit)?;
        let repo = self.inner.repository.to_thread_local();
        let Some(notes_tip) = Self::tip_in(&repo, &self.notes_ref(depot, kind))? else {
            return Ok(None);
        };
        let tree = Self::commit_tree(&repo, notes_tip)?;
        Ok(Self::tree_files(&repo, tree)?.remove(&Self::note_path(commit)))
    }

    async fn attach(
        &self,
        depot: SaveDepotId,
        commit: SaveCommitId,
        kind: SaveAttachmentKind,
        mut source: Box<dyn AsyncRead + Send + Unpin>,
    ) -> Result<AttachmentInfo, Error> {
        let mut bytes = Vec::new();
        source
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| Error::Io {
                path: format!("{} attachment", kind.as_str()),
                source: error,
            })?;
        if kind == SaveAttachmentKind::Note {
            std::str::from_utf8(&bytes).map_err(|error| {
                Error::HistoryConflict(format!("commit note is not UTF-8: {error}"))
            })?;
        }
        let _guard = self.inner.operation.lock().await;
        self.ensure_member(depot, commit)?;
        let repo = self.inner.repository.to_thread_local();
        let reference = self.notes_ref(depot, kind);
        let parent = Self::tip_in(&repo, &reference)?;
        let root = match parent {
            Some(parent) => repo
                .find_tree(Self::commit_tree(&repo, parent)?)
                .map_err(|error| Error::git("read notes tree", error))?,
            None => repo.empty_tree(),
        };
        let blob = repo
            .write_blob(&bytes)
            .map_err(|error| Error::git("write attachment", error))?
            .detach();
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit notes tree", error))?;
        editor
            .upsert(
                Self::note_path(commit),
                gix::objs::tree::EntryKind::Blob,
                blob,
            )
            .map_err(|error| Error::git("add note", error))?;
        let tree = editor
            .write()
            .map_err(|error| Error::git("write notes tree", error))?
            .detach();
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        repo.commit_as(
            signature,
            signature,
            reference,
            format!("Snowglass {} for {commit}", kind.as_str()),
            tree,
            parent,
        )
        .map_err(|error| Error::git("commit attachment note", error))?;
        Ok(AttachmentInfo {
            object_id: SaveCommitId(blob),
            byte_length: bytes.len() as u64,
            media_type: if kind == SaveAttachmentKind::Note {
                Some("text/plain; charset=utf-8")
            } else {
                detect_media_type(&bytes)
            },
        })
    }

    async fn remove_attachment(
        &self,
        depot: SaveDepotId,
        commit: SaveCommitId,
        kind: SaveAttachmentKind,
    ) -> Result<bool, Error> {
        let _guard = self.inner.operation.lock().await;
        self.ensure_member(depot, commit)?;
        let repo = self.inner.repository.to_thread_local();
        let reference = self.notes_ref(depot, kind);
        let Some(parent) = Self::tip_in(&repo, &reference)? else {
            return Ok(false);
        };
        let root = repo
            .find_tree(Self::commit_tree(&repo, parent)?)
            .map_err(|error| Error::git("read notes tree", error))?;
        let path = Self::note_path(commit);
        if !Self::tree_files(&repo, root.id)?.contains_key(&path) {
            return Ok(false);
        }
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit notes tree", error))?;
        editor
            .remove_leaf(path)
            .map_err(|error| Error::git("remove note", error))?;
        let tree = editor
            .write()
            .map_err(|error| Error::git("write notes tree", error))?
            .detach();
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        repo.commit_as(
            signature,
            signature,
            reference,
            format!("Remove Snowglass {} for {commit}", kind.as_str()),
            tree,
            [parent],
        )
        .map_err(|error| Error::git("commit note removal", error))?;
        Ok(true)
    }

    pub fn recently_deleted(&self) -> Result<Vec<DeletedSaveInfo>, Error> {
        let repo = self.inner.repository.to_thread_local();
        let Some(reference) = repo
            .try_find_reference(ARCHIVE_REF)
            .map_err(|error| Error::git("find archive reference", error))?
        else {
            return Ok(Vec::new());
        };
        let mut platform = reference.log_iter();
        let Some(lines) = platform
            .rev()
            .map_err(|error| Error::io(&repo.git_dir().join("logs").join(ARCHIVE_REF), error))?
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
            let repo = self.inner.repository.to_thread_local();
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
            let repo = self.inner.repository.to_thread_local();
            let record = self
                .find_deleted_record(&repo, deletion_id)?
                .ok_or_else(|| {
                    Error::HistoryConflict(format!("unknown or expired deletion {deletion_id}"))
                })?;
            let conflict = Self::tip_in(&repo, &self.save_ref(record.info.depot))?.is_some();
            (record, conflict)
        };
        if conflict {
            self.archive_and_remove_locked(owner, record.info.depot)
                .await?;
        }
        self.materialize_locked(
            owner,
            record.info.depot,
            None,
            Some(record.info.tip.0),
            record.screenshot,
            record.icon,
            record.note,
        )
        .await?;
        let repo = self.inner.repository.to_thread_local();
        self.pulse_archive(
            &repo,
            format!("Snowglass archive restore\n\nSnowglass-Deletion-Id: {deletion_id}\n"),
            std::iter::empty::<gix::ObjectId>(),
        )?;
        Self::commit_info(&repo, record.info.tip.0)
    }

    pub(crate) async fn archive_missing_saves<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        present: &HashSet<SaveDepotId>,
    ) -> Result<(), Error> {
        self.recover(owner).await?;
        let tracked = self.tracked_depots()?;
        let _guard = self.inner.operation.lock().await;
        for depot in tracked {
            if !present.contains(&depot) {
                self.archive_and_remove_locked(owner, depot).await?;
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct DeletedRecord {
    info: DeletedSaveInfo,
    screenshot: Option<gix::ObjectId>,
    icon: Option<gix::ObjectId>,
    note: Option<gix::ObjectId>,
}

enum ArchiveRecord {
    Deleted(DeletedRecord),
    Restored { deletion_id: Uuid },
}

impl SaveVersioning {
    fn parse_trailer<'a>(message: &'a str, key: &str) -> Option<&'a str> {
        message.lines().find_map(|line| line.strip_prefix(key))
    }

    fn archive_record(repo: &gix::Repository, id: gix::ObjectId) -> Result<ArchiveRecord, Error> {
        let commit = repo
            .find_commit(id)
            .map_err(|error| Error::git("find archive record", error))?;
        let message = commit.message_raw_sloppy().to_str_lossy();
        let deletion_id = Self::parse_trailer(&message, "Snowglass-Deletion-Id: ")
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| {
                Error::HistoryConflict("archive record has no deletion id".to_owned())
            })?;
        if message.starts_with("Snowglass archive restore") {
            return Ok(ArchiveRecord::Restored { deletion_id });
        }
        if !message.starts_with("Snowglass archive deletion") {
            return Err(Error::HistoryConflict(
                "not a Snowglass archive record".to_owned(),
            ));
        }
        let depot = Self::parse_trailer(&message, "Snowglass-Depot: ")
            .and_then(|value| u64::from_str_radix(value, 16).ok())
            .map(SaveDepotId)
            .ok_or_else(|| Error::HistoryConflict("archive record has no depot".to_owned()))?;
        let mut parents = commit.parent_ids().map(|id| id.detach());
        let tip = parents
            .next()
            .ok_or_else(|| Error::HistoryConflict("archive record has no save tip".to_owned()))?;
        let screenshot = Self::parse_trailer(&message, "Snowglass-Screenshot: ")
            .and_then(|value| gix::ObjectId::from_hex(value.as_bytes()).ok());
        let icon = Self::parse_trailer(&message, "Snowglass-Icon: ")
            .and_then(|value| gix::ObjectId::from_hex(value.as_bytes()).ok());
        let note = Self::parse_trailer(&message, "Snowglass-Note: ")
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
            screenshot,
            icon,
            note,
        }))
    }

    fn ensure_archive_sentinel(&self, repo: &gix::Repository) -> Result<gix::ObjectId, Error> {
        if let Some(id) = Self::tip_in(repo, ARCHIVE_REF)? {
            return Ok(id);
        }
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        repo.commit_as(
            signature,
            signature,
            ARCHIVE_REF,
            "Snowglass recently-deleted archive sentinel",
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
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        let record = repo
            .new_commit_as(signature, signature, message, repo.empty_tree().id, parents)
            .map_err(|error| Error::git("write archive record", error))?
            .id;
        Self::forced_ref_update(repo, ARCHIVE_REF, record, "snowglass: archive record")?;
        Self::forced_ref_update(repo, ARCHIVE_REF, sentinel, "snowglass: archive sentinel")?;
        Ok(record)
    }

    fn delete_refs(&self, repo: &gix::Repository, depot: SaveDepotId) -> Result<(), Error> {
        let mut edits = Vec::new();
        for name in [
            self.save_ref(depot),
            self.notes_ref(depot, SaveAttachmentKind::Screenshot),
            self.notes_ref(depot, SaveAttachmentKind::Icon),
            self.notes_ref(depot, SaveAttachmentKind::Note),
        ] {
            if let Some(reference) = repo
                .try_find_reference(&name)
                .map_err(|error| Error::git("find reference", error))?
            {
                edits.push(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::MustExistAndMatch(
                            reference.target().to_owned().into(),
                        ),
                        log: RefLog::AndReference,
                    },
                    name: gix::refs::FullName::try_from(name.as_str())
                        .map_err(|error| Error::git("validate reference name", error))?,
                    deref: false,
                });
            }
        }
        repo.edit_references_as(edits, None)
            .map(|_| ())
            .map_err(|error| Error::git("delete save references", error))
    }

    async fn archive_and_remove_locked<R: SaveRepo + ?Sized>(
        &self,
        owner: &R,
        depot: SaveDepotId,
    ) -> Result<(), Error> {
        let Some(tip) = ({
            let repo = self.inner.repository.to_thread_local();
            let Some(tip) = Self::tip_in(&repo, &self.save_ref(depot))? else {
                return Ok(());
            };
            let screenshot = Self::tip_in(
                &repo,
                &self.notes_ref(depot, SaveAttachmentKind::Screenshot),
            )?;
            let icon = Self::tip_in(&repo, &self.notes_ref(depot, SaveAttachmentKind::Icon))?;
            let note = Self::tip_in(&repo, &self.notes_ref(depot, SaveAttachmentKind::Note))?;
            let deletion_id = Uuid::new_v4();
            let mut message = format!(
                "Snowglass archive deletion\n\nSnowglass-Deletion-Id: {deletion_id}\nSnowglass-Platform: {}\nSnowglass-Depot: {:016x}\nSnowglass-Save-Tip: {tip}\n",
                self.inner.platform, depot.0
            );
            let mut parents = vec![tip];
            if let Some(id) = screenshot {
                message.push_str(&format!("Snowglass-Screenshot: {id}\n"));
                parents.push(id);
            }
            if let Some(id) = icon {
                message.push_str(&format!("Snowglass-Icon: {id}\n"));
                parents.push(id);
            }
            if let Some(id) = note {
                message.push_str(&format!("Snowglass-Note: {id}\n"));
                parents.push(id);
            }
            self.pulse_archive(&repo, message, parents)?;
            Some(tip)
        }) else {
            return Ok(());
        };
        self.materialize_locked(owner, depot, Some(tip), None, None, None, None)
            .await
    }

    fn install_save_refs(
        &self,
        repo: &gix::Repository,
        depot: SaveDepotId,
        tip: gix::ObjectId,
        screenshot: Option<gix::ObjectId>,
        icon: Option<gix::ObjectId>,
        note: Option<gix::ObjectId>,
    ) -> Result<(), Error> {
        let mut edits = vec![Self::ref_update(
            &self.save_ref(depot),
            tip,
            "snowglass: install save",
        )?];
        for (kind, value) in [
            (SaveAttachmentKind::Screenshot, screenshot),
            (SaveAttachmentKind::Icon, icon),
            (SaveAttachmentKind::Note, note),
        ] {
            let name = self.notes_ref(depot, kind);
            match value {
                Some(id) => edits.push(Self::ref_update(&name, id, "snowglass: install notes")?),
                None => {
                    if let Some(reference) = repo
                        .try_find_reference(&name)
                        .map_err(|error| Error::git("find notes reference", error))?
                    {
                        edits.push(RefEdit {
                            change: Change::Delete {
                                expected: PreviousValue::MustExistAndMatch(
                                    reference.target().to_owned().into(),
                                ),
                                log: RefLog::AndReference,
                            },
                            name: gix::refs::FullName::try_from(name.as_str())
                                .map_err(|error| Error::git("validate reference name", error))?,
                            deref: false,
                        });
                    }
                }
            }
        }
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        repo.edit_references_as(edits, Some(signature.to_ref(&mut signature_time)))
            .map(|_| ())
            .map_err(|error| Error::git("install save references", error))
    }

    fn ref_update(name: &str, id: gix::ObjectId, message: &str) -> Result<RefEdit, Error> {
        Ok(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: message.into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(id),
            },
            name: gix::refs::FullName::try_from(name)
                .map_err(|error| Error::git("validate reference name", error))?,
            deref: false,
        })
    }

    fn find_deleted_record(
        &self,
        repo: &gix::Repository,
        wanted: Uuid,
    ) -> Result<Option<DeletedRecord>, Error> {
        let Some(reference) = repo
            .try_find_reference(ARCHIVE_REF)
            .map_err(|error| Error::git("find archive", error))?
        else {
            return Ok(None);
        };
        let mut platform = reference.log_iter();
        let Some(lines) = platform
            .rev()
            .map_err(|error| Error::io(&repo.git_dir().join("logs").join(ARCHIVE_REF), error))?
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

fn detect_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"glTF") {
        Some("model/gltf-binary")
    } else {
        None
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
            &self.repo.versioning().inner.repository.to_thread_local(),
            self.id.0,
        )
    }

    pub fn open_attachment(
        &self,
        kind: SaveAttachmentKind,
    ) -> Result<Option<Box<dyn Read + Send>>, Error> {
        self.repo
            .versioning()
            .attachment(self.depot, self.id, kind)
            .map(|bytes| bytes.map(|bytes| Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>))
    }

    pub async fn attach(
        &self,
        kind: SaveAttachmentKind,
        source: Box<dyn AsyncRead + Send + Unpin>,
    ) -> Result<AttachmentInfo, Error> {
        self.repo
            .versioning()
            .attach(self.depot, self.id, kind, source)
            .await
    }

    pub async fn remove_attachment(&self, kind: SaveAttachmentKind) -> Result<bool, Error> {
        self.repo
            .versioning()
            .remove_attachment(self.depot, self.id, kind)
            .await
    }

    pub fn note(&self) -> Result<Option<String>, Error> {
        let Some(bytes) =
            self.repo
                .versioning()
                .attachment(self.depot, self.id, SaveAttachmentKind::Note)?
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
        return SaveVersioning::commit_info(&source_core.inner.repository.to_thread_local(), tip);
    }

    let _guard = destination_core.inner.operation.lock().await;
    destination_core.recover_locked(destination).await?;
    let (tip, screenshot, icon, note, conflict) = {
        let source_repo = source_core.inner.repository.to_thread_local();
        let tip = SaveVersioning::tip_in(&source_repo, &source_core.save_ref(depot))?
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", depot.0)))?;
        let screenshot = SaveVersioning::tip_in(
            &source_repo,
            &source_core.notes_ref(depot, SaveAttachmentKind::Screenshot),
        )?;
        let icon = SaveVersioning::tip_in(
            &source_repo,
            &source_core.notes_ref(depot, SaveAttachmentKind::Icon),
        )?;
        let note = SaveVersioning::tip_in(
            &source_repo,
            &source_core.notes_ref(depot, SaveAttachmentKind::Note),
        )?;
        let destination_repo = destination_core.inner.repository.to_thread_local();
        for root in [Some(tip), screenshot, icon, note].into_iter().flatten() {
            copy_object_graph(&source_repo, &destination_repo, root)?;
        }
        let conflict =
            SaveVersioning::tip_in(&destination_repo, &destination_core.save_ref(depot))?.is_some();
        (tip, screenshot, icon, note, conflict)
    };
    if conflict {
        destination_core
            .archive_and_remove_locked(destination, depot)
            .await?;
    }
    destination_core
        .materialize_locked(destination, depot, None, Some(tip), screenshot, icon, note)
        .await?;
    let destination_repo = destination_core.inner.repository.to_thread_local();
    SaveVersioning::commit_info(&destination_repo, tip)
}

async fn delete_save_locked(owner: &dyn SaveRepo, depot: SaveDepotId) -> Result<(), Error> {
    let core = owner.versioning();
    let _guard = core.inner.operation.lock().await;
    core.archive_and_remove_locked(owner, depot).await
}

fn copy_object_graph(
    source: &gix::Repository,
    destination: &gix::Repository,
    root: gix::ObjectId,
) -> Result<(), Error> {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) || destination.has_object(id) {
            continue;
        }
        let object = source
            .find_object(id)
            .map_err(|error| Error::git("read source object", error))?;
        match object.kind {
            gix::objs::Kind::Commit => {
                let commit = object
                    .try_to_commit_ref()
                    .map_err(|error| Error::git("decode source commit", error))?;
                pending.push(commit.tree());
                pending.extend(commit.parents());
            }
            gix::objs::Kind::Tree => {
                let tree = gix::objs::TreeRef::from_bytes(&object.data, id.kind())
                    .map_err(|error| Error::git("decode source tree", error))?;
                pending.extend(tree.entries.into_iter().map(|entry| entry.oid.to_owned()));
            }
            gix::objs::Kind::Tag => {
                let tag = object
                    .try_to_tag_ref()
                    .map_err(|error| Error::git("decode source tag", error))?;
                pending.push(tag.target());
            }
            gix::objs::Kind::Blob => {}
        }
        let written = destination
            .objects
            .write_buf_with_known_id(object.kind, &object.data, id)
            .map_err(|error| Error::git("write destination object", error))?;
        if written != id {
            return Err(Error::HistoryConflict(format!(
                "object id changed while copying {id}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VolumeStorage;

    #[tokio::test]
    async fn volume_identity_and_labels_are_git_tree_history() {
        let directory = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let storage = VersionedStorage::new(
            DirectoryStorage::new(directory.path()).await.unwrap(),
            "test-platform",
            id,
        )
        .unwrap();

        assert!(storage.volumeinfo().await.unwrap().is_none());
        let created = storage
            .brand(id, "test-platform", Some("Original"))
            .await
            .unwrap();
        assert_eq!(created.label.as_deref(), Some("Original"));
        assert!(storage.storage.volumeinfo().await.unwrap().is_none());

        let repo = storage.history.inner.repository.to_thread_local();
        let first = SaveVersioning::tip_in(&repo, VOLUME_REF).unwrap().unwrap();
        let tree = SaveVersioning::commit_tree(&repo, first).unwrap();
        let values = SaveVersioning::tree_files(&repo, tree).unwrap();
        assert_eq!(values.get("id").unwrap(), id.to_string().as_bytes());
        assert_eq!(values.get("platform").unwrap(), b"test-platform");
        assert_eq!(values.get("label").unwrap(), b"Original");
        assert!(values.contains_key("created"));

        storage.set_label(Some("Renamed")).await.unwrap();
        let second = SaveVersioning::tip_in(&repo, VOLUME_REF).unwrap().unwrap();
        assert_ne!(second, first);
        let commit = repo.find_commit(second).unwrap();
        assert_eq!(commit.parent_ids().next().unwrap().detach(), first);
        assert_eq!(storage.label().await.unwrap().as_deref(), Some("Renamed"));
        assert!(storage.artifacts().summaries().await.unwrap().is_empty());

        drop(commit);
        drop(repo);
        drop(storage);
        let reopened = VersionedStorage::new(
            DirectoryStorage::new(directory.path()).await.unwrap(),
            "test-platform",
            id,
        )
        .unwrap();
        let info = reopened.volumeinfo().await.unwrap().unwrap();
        assert_eq!(info.id, id);
        assert_eq!(info.platform, "test-platform");
        assert_eq!(info.label.as_deref(), Some("Renamed"));
        assert_eq!(info.created, created.created);
    }
}
