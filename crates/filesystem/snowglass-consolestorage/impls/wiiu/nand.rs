use crate::impls::NintendoTitleId;
use crate::impls::wiiu::xml;
use crate::nand::{NandCompletion, VirtualNand};
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher, icons::IconSave,
};
use crate::storage::{ChangeSignal, VolumeOverlay};
use crate::{
    BoxFuture, ComposedStorage, EntryKind, Error, MountableStorage, Permissions, PermissionsTable,
    SaveRepo, SaveVersioning, Storage, VersionedStorage, VolumeStorage, ensure_structure,
    requires_structure,
};
use async_trait::async_trait;
use futures::{FutureExt, StreamExt};
use jiff::Timestamp;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

const COUNTRY_FILE: &str = include_str!("./static/country.txt");
const LANGUAGE_FILE: &str = include_str!("./static/language.txt");

const LAYOUT: PermissionsTable = PermissionsTable::new(&[
    ("", Permissions::all()),
    ("sys", Permissions::READ_EXECUTE),
    ("usr/title", Permissions::READ_EXECUTE),
    ("usr/save", Permissions::all()),
    (".manifest.db", Permissions::empty()),
    (".manifest.db-wal", Permissions::empty()),
]);

pub struct WiiUNand {
    storage: Arc<dyn VolumeStorage>,
}

#[derive(Debug)]
pub struct WiiUSaveRepo {
    root: Arc<VersionedStorage>,
}

impl VolumeOverlay for WiiUSaveRepo {
    fn inner(&self) -> &dyn VolumeStorage {
        self.root.as_ref()
    }

    fn claims(&self, path: &str) -> bool {
        WiiUNand::is_save_path(path)
    }
}

#[async_trait]
impl VirtualNand for WiiUNand {
    fn root(&self) -> Arc<dyn VolumeStorage> {
        self.storage.clone()
    }

    fn platform(&self) -> &str {
        Self::PLATFORM
    }

    fn compose(&self, saves: Arc<dyn SaveRepo>) -> Result<Arc<dyn MountableStorage>, Error> {
        let save_storage: Arc<dyn MountableStorage> = saves;
        let immutable: Arc<dyn MountableStorage> = self.root();
        Ok(Arc::new(ComposedStorage::new(vec![
            save_storage,
            immutable,
        ])?))
    }

    async fn open<St: VolumeStorage + 'static>(storage: St) -> Result<Self, Error> {
        let storage: Arc<dyn VolumeStorage> = Arc::new(storage);
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;

        if metadata.platform != Self::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{:?}` is a {} volume",
                storage, metadata.platform
            )));
        }

        Ok(Self { storage })
    }

    async fn new_empty<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage: Arc<dyn VolumeStorage> = Arc::new(storage);

        ensure_structure!(&*storage as &dyn Storage,
          "sys/",
            "sys/title/0005001b/1005c000/content/country.txt": COUNTRY_FILE,
            "sys/title/0005001b/1005c000/content/language.txt": LANGUAGE_FILE,
          "usr/",
            "usr/title/00050000/", // base
            "usr/title/0005000c/", // dlc
            "usr/title/0005000e/", // update
        )?;

        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;

        Ok(Self { storage })
    }

    fn permissions(&self, path: &str) -> Permissions {
        LAYOUT.get(path)
    }

    fn is_complete(&self, completion_criteria: NandCompletion) -> bool {
        // TODO: verify against mtrees.
        match completion_criteria {
            NandCompletion::Minimal => requires_structure!(
                &*self.storage as &dyn Storage,
                "sys/",
                "sys/title/0005001b/1005c000/content/country.txt",
                "sys/title/0005001b/1005c000/content/language.txt",
                "usr/",
                "usr/title/00050000/",
                "usr/title/0005000c/",
                "usr/title/0005000e/",
            )
            .is_ok(),
            NandCompletion::Full | NandCompletion::Verified => false,
        }
    }

    async fn clone<St: VolumeStorage + 'static>(&self, storage: St) -> Result<Self, Error> {
        todo!()
    }
}

trait WiiUSaveLayout {
    /// Recover the owning title id from a path under [`WiiUNand::SAVE_ROOT`].
    fn from_save_path(path: &str) -> Option<Self>
    where
        Self: Sized;

    fn save_root(&self) -> String;

    fn is_canonical(&self) -> bool;
}

impl WiiUSaveLayout for SaveDepotId {
    fn from_save_path(path: &str) -> Option<Self> {
        let rest = path
            .strip_prefix(WiiUNand::SAVE_ROOT)?
            .trim_start_matches('/');
        let mut parts = rest.split('/');
        let high = u32::from_str_radix(parts.next()?, 16).ok()?;
        let low = u32::from_str_radix(parts.next()?, 16).ok()?;

        Some(NintendoTitleId::new_from_parts(high, low).into())
    }

    fn save_root(&self) -> String {
        let SaveDepotId(title_id) = self;
        format!(
            "{}/{:08x}/{:08x}",
            WiiUNand::SAVE_ROOT,
            (title_id >> 32) as u32 & !0xC,
            *title_id as u32
        )
    }

    fn is_canonical(&self) -> bool {
        let SaveDepotId(title_id) = self;
        (title_id >> 32) as u32 & 0xC == 0
    }
}

impl WiiUNand {
    const SAVE_ROOT: &str = "usr/save";
    pub const PLATFORM: &str = "NINTENDO_WIIU";

    fn is_save_path(path: &str) -> bool {
        path == Self::SAVE_ROOT || path.starts_with("usr/save/")
    }

    pub async fn new_save_repo(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        let storage = Arc::new(storage);
        storage.create_dir_all(Self::SAVE_ROOT)?;
        // Mii Maker save folders {0x500101004A000, 0x500101004A100, 0x500101004A200}.
        storage.create_dir_all("usr/save/00050010/1004a000/user/common/db")?;
        storage.create_dir_all("usr/save/00050010/1004a100/user/common/db")?;
        storage.create_dir_all("usr/save/00050010/1004a200/user/common/db")?;
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Arc::new(WiiUSaveRepo { root: storage }))
    }

    pub async fn open_save_repo(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        let storage = Arc::new(storage);
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if metadata.platform != Self::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{storage:?}` is a {} volume",
                metadata.platform
            )));
        }
        Ok(Arc::new(WiiUSaveRepo { root: storage }))
    }
}

#[async_trait]
impl SaveRepo for WiiUSaveRepo {
    fn versioning(&self) -> &SaveVersioning {
        self.root.versioning()
    }

    fn read_save_icon(
        &self,
        id: SaveDepotId,
    ) -> Result<Option<crate::savegame::icons::SaveIcon>, Error> {
        Ok(Save {
            storage: self.root.as_ref(),
            root: id.save_root(),
        }
        .read_icon())
    }
}

pub(crate) struct Save<'a> {
    pub(crate) storage: &'a dyn Storage,
    pub(crate) root: String,
}

impl WiiUSaveRepo {
    const SAVE_ROOT: &str = WiiUNand::SAVE_ROOT;

    fn describe_save(&self, id: SaveDepotId) -> Result<SaveDescriptor, Error> {
        let root = id.save_root();
        let timestamp = self
            .root
            .read_dir(&root, true)?
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .filter_map(|entry| self.root.metadata(&entry.path).ok())
            .map(|metadata| metadata.mtime)
            .max()
            .unwrap_or_else(Timestamp::now);

        let meta: Option<xml::MetaXml> = self
            .root
            .open_read(&format!("{root}/meta/meta.xml"))
            .ok()
            .and_then(|source| xml::read_from(BufReader::new(source), &root).ok());

        let title = meta
            .as_ref()
            .map(|meta| meta.longname_en.0.clone())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| format!("{:016x}", id.0));

        let description = meta
            .as_ref()
            .map(|meta| meta.publisher_en.0.clone())
            .unwrap_or_default();

        Ok(SaveDescriptor {
            id,
            title,
            description,
            timestamp,
            root_path: root,
        })
    }

    fn is_system_title(title_id: u64) -> bool {
        (title_id >> 32) as u8 & 0x10 != 0
    }

    fn save_ids(&self) -> Vec<SaveDepotId> {
        let storage = &self.root;
        let Ok(highs) = storage.read_dir(Self::SAVE_ROOT, false) else {
            return Vec::new();
        };

        let mut ids = Vec::new();
        for high in highs {
            let Ok(high_bits) = u32::from_str_radix(&high.name, 16) else {
                continue;
            };

            if Self::is_system_title(u64::from(high_bits) << 32) {
                continue;
            }

            let path = format!("{}/{}", Self::SAVE_ROOT, high.name);
            let Ok(lows) = storage.read_dir(&path, false) else {
                continue;
            };

            for low in lows {
                if let Ok(low_bits) = u32::from_str_radix(&low.name, 16) {
                    let id: SaveDepotId =
                        NintendoTitleId::new((u64::from(high_bits) << 32) | u64::from(low_bits))
                            .into();

                    if id.is_canonical() {
                        ids.push(id);
                    }
                }
            }
        }

        ids.sort_by_key(|id| id.0);
        ids
    }
}

impl SaveGameSink for WiiUSaveRepo {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let title_id = package
            .title_id()
            .and_then(|title_id| u64::from_str_radix(title_id.trim_start_matches("0x"), 16).ok())
            .expect("a Wii U package must expose a hexadecimal title ID");
        NintendoTitleId::new(title_id).into()
    }

    fn enumerate_saves(&self) -> SaveDescriptors<'_> {
        Box::new(
            self.save_ids()
                .into_iter()
                .filter_map(|id| self.describe_save(id).ok())
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }

    fn export_save<'a>(&self, save_descriptor: &SaveDepotId) -> Result<SaveDepot, Error> {
        let root = save_descriptor.save_root();
        let save_descriptor = self.describe_save(*save_descriptor)?;

        let mut blobs = Vec::new();
        let mut paths: Vec<String> = self
            .root
            .read_dir(&root, true)?
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .map(|entry| entry.path)
            .collect();
        paths.sort();

        for path in paths {
            let mut bytes = Vec::new();
            self.root
                .open_read(&path)?
                .read_to_end(&mut bytes)
                .map_err(|e| Error::Io {
                    path: path.clone(),
                    source: e,
                })?;

            blobs.push(SaveBlob {
                target_path: PathBuf::from(path),
                blake3: *blake3::hash(&bytes).as_bytes(),
                bytes,
            });
        }

        Ok(SaveDepot {
            save_descriptor,
            blobs,
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let roots = if saves.is_empty() {
            // An empty filter watches every save
            vec![Self::SAVE_ROOT.to_owned()]
        } else {
            let mut roots = Vec::with_capacity(saves.len());
            for id in saves {
                let root = id.save_root();
                self.root.create_dir_all(&root)?;
                roots.push(root);
            }
            roots
        };

        let borrowed: Vec<&str> = roots.iter().map(String::as_str).collect();
        let activity = self.root.watch(&borrowed)?.ok_or_else(|| {
            Error::Unsupported(format!("{} cannot watch saves", WiiUNand::PLATFORM))
        })?;

        let signal = FlushSignal {
            storage: self.root.clone(),
            activity,
            markers: saves
                .iter()
                .map(|id| format!("{}/meta/saveinfo.xml", id.save_root()))
                .collect(),
        };

        Ok(SaveWatcher::new(saves.to_vec(), Box::new(signal)))
    }

    fn install_save<'a>(
        &'a self,
        blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let mut blobs = blobs;
            let mut id = None;

            while let Some(blob) = blobs.next().await {
                let target = blob.target_path.to_string_lossy().replace('\\', "/");
                let expected = blob.blake3;

                let written = self
                    .root
                    .install_file(Box::new(std::io::Cursor::new(blob.bytes)), &target)
                    .await?;

                if written.blake3 != expected {
                    return Err(Error::IntegrityMatchFailed { failed: target });
                }

                id = id.or_else(|| SaveDepotId::from_save_path(&target));
            }

            let id = id.ok_or(Error::UnclaimedNand)?;
            self.describe_save(id)
        })
    }
}

struct FlushSignal {
    storage: Arc<dyn MountableStorage>,
    activity: Box<dyn ChangeSignal>,
    markers: Vec<String>,
}

impl FlushSignal {
    const SAVE_SETTLE: Duration = Duration::from_secs(2);
    fn markers(&self) -> Vec<Option<Timestamp>> {
        self.markers
            .iter()
            .map(|path| self.storage.metadata(path).ok().map(|meta| meta.mtime))
            .collect()
    }
}

impl ChangeSignal for FlushSignal {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.activity.changed().await;
            let before = self.markers();

            loop {
                tokio::time::sleep(Self::SAVE_SETTLE).await;
                let writing = self.activity.changed().now_or_never().is_some();

                if self.markers() != before || !writing {
                    return;
                }
            }
        })
    }
}
