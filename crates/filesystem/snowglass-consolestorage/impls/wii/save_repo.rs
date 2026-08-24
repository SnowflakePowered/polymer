use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use jiff::Timestamp;
use uuid::Uuid;

use super::nand::WiiNand;
use super::{data_path, save_id_from_path};
use crate::savegame::icons::{IconSave, SaveIcon};
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher,
};
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, EntryKind, Error, SaveRepo, SaveVersioning, Storage, VersionedStorage, VolumeStorage,
};

#[derive(Debug)]
pub struct WiiSaveRepo {
    root: Arc<VersionedStorage>,
}

pub(crate) struct Save<'a> {
    pub(crate) storage: &'a dyn Storage,
    pub(crate) root: String,
}

impl WiiSaveRepo {
    const HIDDEN_RESOURCES: [&'static str; 3] = [
        "shared2/menu/FaceLib",
        "shared2/sys/SYSCONF",
        "title/00000001/00000002/data",
    ];

    fn is_hidden_resource(path: &str) -> bool {
        Self::HIDDEN_RESOURCES
            .iter()
            .any(|root| path == *root || path.starts_with(&format!("{root}/")))
    }

    pub(crate) async fn create(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        let root = Arc::new(storage);
        root.create_dir_all("title")?;
        root.create_dir_all("shared2/menu/FaceLib")?;
        root.create_dir_all("shared2/sys")?;
        root.create_dir_all("title/00000001/00000002/data")?;
        root.brand(id, WiiNand::PLATFORM, label.as_deref()).await?;
        Ok(Arc::new(Self { root }))
    }

    pub(crate) async fn open(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        let root = Arc::new(storage);
        let metadata = root.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if metadata.platform != WiiNand::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{root:?}` is a {} volume",
                metadata.platform
            )));
        }
        Ok(Arc::new(Self { root }))
    }

    fn describe_save(&self, id: SaveDepotId) -> Result<SaveDescriptor, Error> {
        let root = data_path(id.0);
        let timestamp = self
            .root
            .read_dir(&root, true)?
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .map(|entry| entry.metadata.mtime)
            .max()
            .unwrap_or_else(Timestamp::now);
        let banner = crate::savegame::icons::wii::Banner::read(self.root.as_ref(), &root);
        Ok(SaveDescriptor {
            id,
            title: banner
                .as_ref()
                .map(|banner| banner.title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| format!("{:016x}", id.0)),
            description: banner.map_or_else(String::new, |banner| banner.description),
            timestamp,
            root_path: root,
        })
    }

    fn save_ids(&self) -> Vec<SaveDepotId> {
        let Ok(entries) = self.root.read_dir("title", true) else {
            return Vec::new();
        };
        let mut ids = entries
            .filter(|entry| entry.metadata.kind == EntryKind::Directory)
            .filter_map(|entry| save_id_from_path(&entry.path))
            .filter(|id| {
                self.root
                    .read_dir(&data_path(id.0), true)
                    .is_ok_and(|mut entries| entries.next().is_some())
            })
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn fingerprint(&self, selected: &[SaveDepotId]) -> Vec<(String, u64, Timestamp)> {
        let ids = if selected.is_empty() {
            self.save_ids()
        } else {
            selected.to_vec()
        };
        let mut state = Vec::new();
        for id in ids {
            let root = data_path(id.0);
            if let Ok(entries) = self.root.read_dir(&root, true) {
                state.extend(
                    entries
                        .filter(|entry| entry.metadata.kind == EntryKind::File)
                        .map(|entry| (entry.path, entry.metadata.len, entry.metadata.mtime)),
                );
            }
        }
        state.sort_by(|left, right| left.0.cmp(&right.0));
        state
    }
}

impl VolumeOverlay for WiiSaveRepo {
    fn inner(&self) -> &dyn VolumeStorage {
        self.root.as_ref()
    }

    fn claims(&self, path: &str) -> bool {
        Self::is_hidden_resource(path)
            || save_id_from_path(path).is_some()
            || (path.starts_with("title") && self.root.exists(path))
    }
}

#[async_trait]
impl SaveRepo for WiiSaveRepo {
    fn versioning(&self) -> &SaveVersioning {
        self.root.versioning()
    }

    fn read_save_icon(&self, id: SaveDepotId) -> Result<Option<SaveIcon>, Error> {
        Ok(Save {
            storage: self.root.as_ref(),
            root: data_path(id.0),
        }
        .read_icon())
    }
}

impl SaveGameSink for WiiSaveRepo {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        if let Some(id) = package
            .title_id()
            .and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok())
        {
            return SaveDepotId(id);
        }
        let serial = package
            .serial()
            .expect("a Wii package must expose a title ID or game ID")
            .as_bytes();
        assert!(serial.len() >= 4, "a Wii game ID must contain four bytes");
        let low = u32::from_be_bytes(serial[..4].try_into().unwrap());
        SaveDepotId((0x0001_0000_u64 << 32) | u64::from(low))
    }

    fn enumerate_saves(&self) -> SaveDescriptors<'_> {
        Box::new(
            self.save_ids()
                .into_iter()
                .filter_map(|id| self.describe_save(id).ok()),
        )
    }

    fn export_save<'a>(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        let root = data_path(id.0);
        let mut paths = self
            .root
            .read_dir(&root, true)?
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
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
        mut blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let mut id = None;
            while let Some(blob) = blobs.next().await {
                let target = blob.target_path.to_string_lossy().replace('\\', "/");
                let candidate = save_id_from_path(&target).ok_or_else(|| {
                    Error::InvalidPath(format!("`{target}` is not a Wii save path"))
                })?;
                if id.is_some_and(|id| id != candidate) {
                    return Err(Error::InvalidPath(
                        "a Wii save depot cannot contain multiple title IDs".to_owned(),
                    ));
                }
                id = Some(candidate);
                let expected = blob.blake3;
                let installed = self
                    .root
                    .install_file(Box::new(std::io::Cursor::new(blob.bytes)), &target)
                    .await?;
                if installed.blake3 != expected {
                    return Err(Error::IntegrityMatchFailed { failed: target });
                }
            }
            self.describe_save(
                id.ok_or_else(|| Error::InvalidPath("a Wii save depot is empty".to_owned()))?,
            )
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let roots = if saves.is_empty() {
            vec!["title".to_owned()]
        } else {
            saves.iter().map(|id| data_path(id.0)).collect()
        };
        for root in &roots {
            self.root.create_dir_all(root)?;
        }
        let watched = roots.iter().map(String::as_str).collect::<Vec<_>>();
        let activity = self.root.watch(&watched)?.ok_or_else(|| {
            Error::Unsupported(format!("{} cannot watch saves", WiiNand::PLATFORM))
        })?;
        let selected = saves.to_vec();
        let root = self.root.clone();
        Ok(SaveWatcher::with_fingerprint(
            saves.to_vec(),
            activity,
            Duration::from_secs(2),
            move || {
                let owner = Self { root: root.clone() };
                owner.fingerprint(&selected)
            },
        ))
    }
}
