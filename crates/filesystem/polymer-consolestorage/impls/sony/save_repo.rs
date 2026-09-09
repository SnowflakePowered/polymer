use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use orbis_unpkg::Psf;
use uuid::Uuid;

use super::TitleId;

use crate::savegame::icons::SaveIcon;
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher,
};
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, EntryKind, Error, SaveCommitOperation, SaveRepo, SaveVersioning, Storage,
    VersionedStorage, VolumeStorage,
};

#[async_trait]
pub trait SaveLayout: std::fmt::Debug + Send + Sync + 'static {
    const PLATFORM: &'static str;
    const SAVE_DIRECTORY: &'static str;
    const SUB_TITLE: &'static str;
    const DETAIL: &'static str;
    const HAS_STAGING: bool;
    async fn initialize(root: &VersionedStorage, profile_root: &str) -> Result<(), Error>;
    fn claims(profile_root: &str, save_root: &str, path: &str) -> bool;
    fn package_id(package: &Package) -> Option<&str>;
}

const MAX_SFO_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct SonySaveRepo<L: SaveLayout> {
    layout: std::marker::PhantomData<L>,
    root: Arc<VersionedStorage>,
    profile_root: String,
    save_root: String,
}

impl<L: SaveLayout> SonySaveRepo<L> {
    pub(crate) async fn create(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
        profile_root: &str,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        let root = Arc::new(storage);
        let save_root = format!("{profile_root}/{}", L::SAVE_DIRECTORY);
        root.create_dir_all(&save_root)?;
        L::initialize(root.as_ref(), profile_root).await?;
        root.brand(id, L::PLATFORM, label.as_deref()).await?;
        Ok(Arc::new(Self {
            layout: std::marker::PhantomData,
            root,
            profile_root: profile_root.to_owned(),
            save_root,
        }))
    }

    pub(crate) async fn open(
        storage: VersionedStorage,
        profile_root: &str,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        let root = Arc::new(storage);
        let metadata = root.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if metadata.platform != L::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{root:?}` is a {} volume",
                metadata.platform
            )));
        }
        Ok(Arc::new(Self {
            layout: std::marker::PhantomData,
            root,
            profile_root: profile_root.to_owned(),
            save_root: format!("{profile_root}/{}", L::SAVE_DIRECTORY),
        }))
    }

    fn canonical_directories(&self, id: SaveDepotId) -> Vec<String> {
        let Ok(entries) = self.root.read_dir(&self.save_root, false) else {
            return Vec::new();
        };
        let mut directories = entries
            .filter(|entry| entry.metadata.kind == EntryKind::Directory)
            .filter(|entry| {
                TitleId::from_directory(&entry.name).is_some_and(|title| title.depot == id)
            })
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        directories.sort();
        directories
    }

    fn save_ids(&self) -> Vec<SaveDepotId> {
        let Ok(entries) = self.root.read_dir(&self.save_root, false) else {
            return Vec::new();
        };
        let mut ids = entries
            .filter(|entry| entry.metadata.kind == EntryKind::Directory)
            .filter_map(|entry| TitleId::from_directory(&entry.name).map(|title| title.depot))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn has_staging(&self) -> bool {
        if !L::HAS_STAGING {
            return false;
        }
        self.root
            .read_dir(&self.save_root, false)
            .is_ok_and(|mut entries| {
                entries.any(|entry| {
                    entry.metadata.kind == EntryKind::Directory && entry.name.starts_with('.')
                })
            })
    }

    fn read_sfo(&self, directory: &str) -> Option<Psf> {
        let path = format!("{directory}/PARAM.SFO");
        let metadata = self.root.metadata(&path).ok()?;
        if metadata.len > MAX_SFO_BYTES {
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len as usize);
        self.root
            .open_read(&path)
            .ok()?
            .take(MAX_SFO_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_SFO_BYTES {
            return None;
        }
        if bytes.len() < 20 {
            return None;
        }
        let count = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
        if count > (bytes.len() - 20) / 16 {
            return None;
        }
        Psf::parse(&bytes).ok()
    }

    fn describe_save(&self, id: SaveDepotId) -> Result<SaveDescriptor, Error> {
        let directories = self.canonical_directories(id);
        if directories.is_empty() {
            return Err(Error::InvalidPath(format!(
                "Sony save family {:09x} is empty",
                id.0
            )));
        }
        let timestamp = directories
            .iter()
            .filter_map(|directory| self.root.read_dir(directory, true).ok())
            .flatten()
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .map(|entry| entry.metadata.mtime)
            .max()
            .unwrap_or_else(Timestamp::now);
        let sfo = directories
            .iter()
            .find_map(|directory| self.read_sfo(directory));
        let title = sfo
            .as_ref()
            .and_then(|sfo| sfo.get_string("TITLE"))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                directories[0]
                    .rsplit('/')
                    .next()
                    .unwrap_or("Sony save")
                    .to_owned()
            });
        let description = sfo
            .as_ref()
            .and_then(|sfo| {
                sfo.get_string(L::SUB_TITLE)
                    .or_else(|| sfo.get_string(L::DETAIL))
            })
            .unwrap_or_default()
            .to_owned();
        Ok(SaveDescriptor {
            id,
            title,
            description,
            timestamp,
            root_path: self.save_root.clone(),
        })
    }
}

impl<L: SaveLayout> VolumeOverlay for SonySaveRepo<L> {
    fn inner(&self) -> &dyn VolumeStorage {
        self.root.as_ref()
    }

    fn claims(&self, path: &str) -> bool {
        L::claims(&self.profile_root, &self.save_root, path)
    }
}

#[async_trait]
impl<L: SaveLayout> SaveRepo for SonySaveRepo<L> {
    fn versioning(&self) -> &SaveVersioning {
        self.root.versioning()
    }

    fn read_save_icon(&self, id: SaveDepotId) -> Result<Option<SaveIcon>, Error> {
        for directory in self.canonical_directories(id) {
            let path = format!("{directory}/ICON0.PNG");
            let mut bytes = Vec::new();
            if self
                .root
                .open_read(&path)
                .and_then(|mut source| {
                    source.read_to_end(&mut bytes).map_err(|source| Error::Io {
                        path: path.clone(),
                        source,
                    })
                })
                .is_ok()
            {
                if let Ok(icon) = SaveIcon::decode(&bytes) {
                    return Ok(Some(icon));
                }
            }
        }
        Ok(None)
    }

    async fn checkpoint_saves(&self) -> Result<bool, Error> {
        if self.has_staging() {
            return self.versioning().checkpoint_profile();
        }
        let saves = self.save_ids();
        let present = saves.iter().copied().collect::<HashSet<_>>();
        let mut changed = false;
        for save in saves {
            changed |= self
                .versioning()
                .checkpoint(self, save, SaveCommitOperation::Autosave)
                .await?
                .is_some();
        }
        changed |= self
            .versioning()
            .archive_missing_saves(self, &present)
            .await?;
        changed |= self.versioning().checkpoint_profile()?;
        Ok(changed)
    }
}

impl<L: SaveLayout> SaveGameSink for SonySaveRepo<L> {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let raw = L::package_id(package)
            .expect("a Sony package must expose a product serial or title ID");
        let normalized = if raw.len() == 10 && raw.as_bytes()[4] == b'-' {
            format!("{}{}", &raw[..4], &raw[5..])
        } else {
            raw.to_owned()
        }
        .to_ascii_uppercase();
        TitleId::parse(&normalized)
            .expect("a Sony product code must contain exactly four letters and five digits")
            .depot
    }

    fn enumerate_saves(&self) -> SaveDescriptors<'_> {
        Box::new(
            self.save_ids()
                .into_iter()
                .filter_map(|id| self.describe_save(id).ok()),
        )
    }

    fn export_save<'a>(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        if self.has_staging() {
            return Err(Error::HistoryConflict(format!(
                "Sony save family {:09x} is being published",
                id.0
            )));
        }
        let mut paths = Vec::new();
        for directory in self.canonical_directories(*id) {
            paths.extend(
                self.root
                    .read_dir(&directory, true)?
                    .filter(|entry| entry.metadata.kind == EntryKind::File)
                    .map(|entry| entry.path),
            );
        }
        paths.sort();
        let mut blobs = Vec::with_capacity(paths.len());
        for path in paths {
            let mut bytes = Vec::new();
            self.root
                .open_read(&path)?
                .read_to_end(&mut bytes)
                .map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
            blobs.push(SaveBlob {
                target_path: PathBuf::from(path),
                blake3: *blake3::hash(&bytes).as_bytes(),
                bytes,
            });
        }
        Ok(SaveDepot {
            save_descriptor: self.describe_save(*id)?,
            blobs,
        })
    }

    fn install_save<'a>(
        &'a self,
        blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let import = super::import::SaveImport::read(blobs, &self.save_root).await?;
            if self.has_staging() {
                return Err(Error::HistoryConflict(
                    "Sony savedata is being published".to_owned(),
                ));
            }
            for (target, blob) in import.files {
                self.root
                    .install_file(Box::new(std::io::Cursor::new(blob.bytes)), &target)
                    .await?;
            }
            self.describe_save(import.title.depot)
        })
    }
    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        self.root.create_dir_all(&self.save_root)?;
        let activity = self
            .root
            .watch()?
            .ok_or_else(|| Error::Unsupported(format!("{} cannot watch saves", L::PLATFORM)))?;
        Ok(SaveWatcher::with_settle(
            saves.to_vec(),
            activity,
            Duration::from_secs(2),
        ))
    }
}
