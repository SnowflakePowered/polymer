use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use uuid::Uuid;

use super::{
    CtrNand,
    layout::{Archive, Layout},
    smdh::ExtdataIcon,
};
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher, icons::SaveIcon,
};
use crate::storage::VolumeOverlay;
use crate::{
    AttachmentScope, BoxFuture, EntryKind, Error, SaveAttachmentKind, SaveRepo, SaveVersioning,
    Storage, VersionedStorage, VolumeStorage,
};

/// Regular saves and SD extdata have independent histories; shared NAND data is
/// retained in the profile checkpoint. Extdata can be shared by several titles.
#[derive(Debug)]
pub struct CtrSaveRepo {
    root: Arc<VersionedStorage>,
}

impl CtrSaveRepo {
    pub async fn create(
        storage: VersionedStorage,
        id: Uuid,
        platform: &str,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        if !CtrNand::supports(platform) {
            return Err(Error::Unsupported(platform.to_owned()));
        }
        let root = Arc::new(storage);
        for path in [
            format!("{}/title", Layout::SD),
            format!("{}/extdata/00000000", Layout::SD),
            format!("{}/sysdata", Layout::NAND_DATA),
            format!("{}/extdata/00048000", Layout::NAND_DATA),
        ] {
            root.create_dir_all(&path)?;
        }
        root.brand(id, platform, label.as_deref()).await?;
        Ok(Arc::new(Self { root }))
    }

    pub async fn open(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if !CtrNand::supports(&metadata.platform) {
            return Err(Error::Unsupported(metadata.platform));
        }
        Ok(Arc::new(Self {
            root: Arc::new(storage),
        }))
    }

    fn archives(&self) -> BTreeMap<SaveDepotId, String> {
        let mut archives = BTreeMap::new();
        let Ok(entries) = self.root.read_dir(Layout::SD, true) else {
            return archives;
        };
        for entry in entries.filter(|entry| entry.metadata.kind == EntryKind::File) {
            if let Some(archive) = Archive::from_path(&entry.path) {
                // Keep the on-disk spelling: Azahar emits uppercase extdata IDs
                // in some paths and lowercase in others.
                let depth = match archive {
                    Archive::Title(_) => 8,
                    Archive::Extdata(_) => 7,
                };
                archives.entry(archive.id()).or_insert_with(|| {
                    entry
                        .path
                        .split('/')
                        .take(depth)
                        .collect::<Vec<_>>()
                        .join("/")
                });
            }
        }
        archives
    }

    fn archive_root(&self, id: SaveDepotId) -> Result<String, Error> {
        let archive = Archive::from_id(id)
            .ok_or_else(|| Error::InvalidPath(format!("invalid 3DS save ID {:016x}", id.0)))?;
        let canonical = archive.root();
        let mut resolved = String::new();
        // Resolve the short directory chain rather than rescanning every save
        // on each checkpoint. Preserve the case of the archive's hex IDs.
        for component in canonical.split('/') {
            let name = self
                .root
                .read_dir(&resolved, false)
                .ok()
                .and_then(|mut entries| {
                    entries.find(|entry| entry.name.eq_ignore_ascii_case(component))
                })
                .map(|entry| entry.name)
                .unwrap_or_else(|| component.to_owned());
            if !resolved.is_empty() {
                resolved.push('/');
            }
            resolved.push_str(&name);
        }
        Ok(resolved)
    }

    fn describe_save(&self, id: SaveDepotId, root: String) -> Result<SaveDescriptor, Error> {
        let timestamp = self
            .root
            .read_dir(&root, true)?
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .map(|entry| entry.metadata.mtime)
            .max()
            .unwrap_or_else(Timestamp::now);
        let metadata = matches!(Archive::from_id(id), Some(Archive::Extdata(_)))
            .then(|| ExtdataIcon::read(self.root.as_ref(), &root))
            .flatten();
        let (title, description) = metadata.map(|icon| icon.description()).unwrap_or_default();
        let title = match title.is_empty() {
            true => self
                .root
                .versioning()
                .attachment(id, AttachmentScope::History, SaveAttachmentKind::Title)?
                .and_then(|title| String::from_utf8(title).ok())
                .unwrap_or_else(|| format!("{:016x}", id.0)),
            false => title,
        };
        Ok(SaveDescriptor {
            id,
            title,
            description,
            timestamp,
            root_path: root,
        })
    }
}

impl VolumeOverlay for CtrSaveRepo {
    fn inner(&self) -> &dyn VolumeStorage {
        self.root.as_ref()
    }

    fn claims(&self, path: &str) -> bool {
        if !Layout::valid_path(path) {
            return false;
        }
        Layout::profile_path(path)
            || Archive::from_path(path).is_some()
            || (path.starts_with("sdmc/") && self.root.exists(path))
    }
}

#[async_trait]
impl SaveRepo for CtrSaveRepo {
    fn versioning(&self) -> &SaveVersioning {
        self.root.versioning()
    }

    fn read_save_icon(&self, id: SaveDepotId) -> Result<Option<SaveIcon>, Error> {
        if !matches!(Archive::from_id(id), Some(Archive::Extdata(_))) {
            return Ok(None);
        }
        Ok(ExtdataIcon::read(self.root.as_ref(), &self.archive_root(id)?).map(|icon| icon.image()))
    }

    /// Game saves carry no SMDH; the 3DS data manager shows the title's own.
    async fn describe_launch(&self, package: &Package, path: &Path) -> Result<(), Error> {
        let titled = package
            .title_id()
            .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
            .and_then(|id| Archive::from_id(SaveDepotId(id)))
            .is_some_and(|archive| matches!(archive, Archive::Title(_)));
        let Some(smdh) = titled.then(|| ExtdataIcon::from_game(path)).flatten() else {
            return Ok(());
        };
        let id = self.infer_save_depot(package);
        let versioning = self.root.versioning();
        for (kind, bytes) in [
            (SaveAttachmentKind::Icon, smdh.image().encode()?),
            (SaveAttachmentKind::Title, smdh.description().0.into_bytes()),
        ] {
            if bytes.is_empty()
                || versioning
                    .attachment(id, AttachmentScope::History, kind)?
                    .as_ref()
                    == Some(&bytes)
            {
                continue;
            }
            versioning
                .attach(
                    id,
                    AttachmentScope::History,
                    kind,
                    Box::new(Cursor::new(bytes)),
                )
                .await?;
        }
        Ok(())
    }

    /// Wii and 3DS data management count 128 KiB blocks.
    fn blocks(&self, id: SaveDepotId) -> Result<Option<u64>, Error> {
        let bytes = self
            .export_save(&id)?
            .blobs
            .iter()
            .map(|blob| blob.bytes.len() as u64)
            .sum::<u64>();
        Ok(Some(bytes.div_ceil(128 * 1024)))
    }
}

impl SaveGameSink for CtrSaveRepo {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let id = package
            .title_id()
            .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
            .expect("a 3DS package must expose a hexadecimal title ID");
        // Updates/DLC refer to the same base application, preserving New 3DS bits.
        let id = match id >> 32 {
            0x0004000e | 0x0004008c => (0x00040000_u64 << 32) | (id & 0xffffffff),
            _ => id,
        };
        Archive::from_id(SaveDepotId(id))
            .filter(|archive| matches!(archive, Archive::Title(_)))
            .expect("a 3DS package must expose a CTR title ID")
            .id()
    }

    fn enumerate_saves(&self) -> SaveDescriptors<'_> {
        Box::new(
            self.archives()
                .into_iter()
                .filter_map(|(id, root)| self.describe_save(id, root).ok()),
        )
    }

    fn export_save(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        let descriptor = self.describe_save(*id, self.archive_root(*id)?)?;
        let mut paths = self
            .root
            .read_dir(&descriptor.root_path, true)?
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
            save_descriptor: descriptor,
            blobs,
        })
    }

    fn install_save<'a>(
        &'a self,
        blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let import = super::import::SaveImport::read(blobs).await?;
            for (path, bytes) in import.files {
                self.root
                    .install_file(Box::new(Cursor::new(bytes)), &path)
                    .await?;
            }
            self.describe_save(import.archive.id(), self.archive_root(import.archive.id())?)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        // Watch the profile itself. Creating an archive directory here would
        // suppress Azahar's NotFormatted result on the first launch.
        let activity = self
            .root
            .watch()?
            .ok_or_else(|| Error::Unsupported("3DS profile cannot watch saves".to_owned()))?;
        Ok(SaveWatcher::with_settle(
            saves.to_vec(),
            activity,
            Duration::from_secs(2),
        ))
    }
}
