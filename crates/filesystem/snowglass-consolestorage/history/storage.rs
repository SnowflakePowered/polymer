use std::collections::BTreeMap;
use std::sync::Arc;

use jiff::Timestamp;
use tokio::io::AsyncRead;
use uuid::Uuid;

use super::{SaveVersioning, VOLUME_REF};
use crate::{
    ArtifactRegistry, ArtifactSummary, BoxFuture, ChangeSignal, DirectoryStorage, Error,
    InstallResult, Metadata, MountableStorage, ReadDir, Storage, StorageFile, VolumeInfo,
    VolumeStorage,
};

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

impl SaveVersioning {
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
}
