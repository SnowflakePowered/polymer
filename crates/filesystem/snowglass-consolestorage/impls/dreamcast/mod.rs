mod raw;
pub mod vmi;
pub mod vms;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use atomic_write_file::AtomicWriteFile;
use futures::StreamExt;
use jiff::Timestamp;
use uuid::Uuid;

use self::vmi::Vmi;
use self::vms::Save;
use crate::impls::{
    DreamcastProductId,
    memorycard::{MemoryCardStorage, RawMemoryCardImage},
};
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher,
};
use crate::savegroups::{DREAMCAST_SAVE_GROUPS, TARGET_CAPACITY_PERCENT};
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, DirectoryStorage, EntryKind, Error, MemoryCard, MemoryCardView, SaveRepo, Storage,
    VolumeStorage,
};

const SAVE_ROOT: &str = "card";
#[derive(Clone)]
pub struct DreamcastMemoryCard {
    storage: Arc<DirectoryStorage>,
}

impl VolumeOverlay for DreamcastMemoryCard {
    fn inner(&self) -> &dyn VolumeStorage {
        self.storage.as_ref()
    }
}

#[async_trait::async_trait]
impl SaveRepo for DreamcastMemoryCard {}

impl DreamcastMemoryCard {
    pub const PLATFORM: &str = "SEGA_DC";

    pub async fn new_empty(
        storage: DirectoryStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        storage.create_dir_all(SAVE_ROOT)?;
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Self { storage })
    }

    pub async fn open(storage: DirectoryStorage) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        let identity = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if identity.platform != Self::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{storage:?}` is a {} volume",
                identity.platform
            )));
        }
        storage.create_dir_all(SAVE_ROOT)?;
        Ok(Self { storage })
    }

    fn product_path(product: DreamcastProductId) -> String {
        format!("{SAVE_ROOT}/{product}")
    }

    fn save_paths(product: DreamcastProductId, save: &Save<'_>) -> (String, String) {
        Self::vmi_paths(product, save.vmi())
    }

    fn vmi_paths(product: DreamcastProductId, vmi: &Vmi<'_>) -> (String, String) {
        let root = Self::product_path(product);
        let resource = vmi.resource_stem();
        (
            format!("{root}/{resource}.vmi"),
            format!("{root}/{resource}.vms"),
        )
    }

    fn saves(&self) -> Vec<(DreamcastProductId, Save<'static>)> {
        let Ok(products) = self.storage.read_dir(SAVE_ROOT, false) else {
            return Vec::new();
        };
        let mut saves = Vec::new();
        for product_entry in products.filter(|entry| entry.metadata.kind == EntryKind::Directory) {
            let Some(product) = DreamcastProductId::new(product_entry.name.as_bytes()) else {
                continue;
            };
            let Ok(entries) = self.storage.read_dir(&product_entry.path, false) else {
                continue;
            };
            for entry in entries.filter(|entry| {
                entry.metadata.kind == EntryKind::File
                    && entry.name.to_ascii_lowercase().ends_with(".vmi")
            }) {
                let stem = &entry.name[..entry.name.len() - 4];
                let vms_path = format!("{}/{stem}.vms", product_entry.path);
                let mut vmi = Vec::new();
                let mut vms = Vec::new();
                let parsed = self
                    .storage
                    .open_read(&entry.path)
                    .and_then(|mut file| {
                        file.read_to_end(&mut vmi)
                            .map_err(|source| Error::io(self.storage.root(), source))?;
                        Ok(())
                    })
                    .and_then(|()| self.storage.open_read(&vms_path))
                    .and_then(|mut file| {
                        file.read_to_end(&mut vms)
                            .map_err(|source| Error::io(self.storage.root(), source))?;
                        Ok(())
                    })
                    .and_then(|()| Save::new(vmi, vms).map_err(Error::from));
                if let Ok(save) = parsed
                    && save.vmi().resource_stem().eq_ignore_ascii_case(stem)
                {
                    saves.push((product, save));
                }
            }
        }
        saves.sort_by(|(left_product, left), (right_product, right)| {
            left_product
                .cmp(right_product)
                .then_with(|| left.vmi().resource_name().cmp(right.vmi().resource_name()))
        });
        saves
    }

    fn project(
        &self,
        primary: &[SaveDepotId],
    ) -> Result<(DreamcastProductId, Vec<(DreamcastProductId, Save<'static>)>), Error> {
        let primary = primary
            .iter()
            .copied()
            .map(|id| {
                DreamcastProductId::try_from(id).map_err(|_| {
                    Error::Unsupported(
                        "a Dreamcast primary must be a Dreamcast product ID".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mounted_product = primary.first().copied().ok_or_else(|| {
            Error::Unsupported("a Dreamcast projection requires a primary game".to_owned())
        })?;
        let mut required = primary
            .iter()
            .copied()
            .map(SaveDepotId::from)
            .map(|id| id.0)
            .collect::<HashSet<_>>();
        required.extend(
            DREAMCAST_SAVE_GROUPS.members(
                &primary
                    .iter()
                    .copied()
                    .map(SaveDepotId::from)
                    .collect::<Vec<_>>(),
            ),
        );
        let mut groups = BTreeMap::<DreamcastProductId, Vec<Save<'static>>>::new();
        for (product, save) in self.saves() {
            groups.entry(product).or_default().push(save);
        }

        let mut groups = groups.into_iter().collect::<Vec<_>>();
        groups.sort_by_key(|(product, _)| {
            (!required.contains(&SaveDepotId::from(*product).0), *product)
        });
        let target = raw::card::USER_BLOCKS * TARGET_CAPACITY_PERCENT / 100;
        let mut blocks = 0u32;
        let mut entries = 0usize;
        let mut identities = HashSet::new();
        let mut projected = Vec::new();
        for (product, saves) in groups {
            let preferred = required.contains(&SaveDepotId::from(product).0);
            let group_blocks = saves
                .iter()
                .map(|save| u32::try_from(save.block_count()).unwrap())
                .sum::<u32>();
            let group_identities = saves.iter().map(Save::identity).collect::<HashSet<_>>();
            let duplicate = group_identities.len() != saves.len()
                || group_identities
                    .iter()
                    .any(|identity| identities.contains(identity));
            let fits = blocks + group_blocks <= raw::card::USER_BLOCKS
                && entries + saves.len() <= raw::card::USER_BLOCKS as usize;
            if preferred && (!fits || duplicate) {
                return Err(Error::Unsupported(
                    "the required Dreamcast save groups do not fit on one VMU".to_owned(),
                ));
            }
            if !fits || duplicate || (!preferred && blocks + group_blocks > target) {
                continue;
            }
            blocks += group_blocks;
            entries += saves.len();
            identities.extend(group_identities);
            projected.extend(saves.into_iter().map(|save| (product, save)));
        }
        Ok((mounted_product, projected))
    }

    fn reconcile(
        &self,
        primary: DreamcastProductId,
        projected: &HashMap<[u8; 12], (DreamcastProductId, Vmi<'static>)>,
        saves: &[Save<'_>],
    ) -> Result<(), Error> {
        let mut decoded = Vec::with_capacity(saves.len());
        let mut identities = HashSet::new();
        let mut paths = HashSet::new();
        for save in saves {
            if !identities.insert(save.identity()) {
                return Err(Error::Unsupported(
                    "a Dreamcast VMU contains duplicate native filenames".to_owned(),
                ));
            }
            let (product, vmi) = match projected.get(&save.identity()) {
                Some((product, original)) => (*product, original.updated_from(save.vmi())?),
                None => (primary, save.vmi().clone().into_owned()),
            };
            let save = Save::from_parts(vmi, save.vms().to_vec())?;
            let pair = Self::save_paths(product, &save);
            if !paths.insert(pair.clone()) {
                return Err(Error::Unsupported(
                    "a Dreamcast VMU contains colliding VMI resource names".to_owned(),
                ));
            }
            decoded.push((product, save, pair));
        }

        for (_, save, (vmi_path, vms_path)) in &decoded {
            self.write_if_changed(vms_path, save.vms())?;
            self.write_if_changed(vmi_path, save.vmi().as_bytes())?;
        }
        for (identity, (product, vmi)) in projected {
            if !identities.contains(identity) {
                let (vmi_path, vms_path) = Self::vmi_paths(*product, vmi);
                self.storage.remove(&vmi_path)?;
                self.storage.remove(&vms_path)?;
            }
        }
        Ok(())
    }

    fn write_if_changed(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        if self
            .storage
            .metadata(path)
            .is_ok_and(|metadata| metadata.len == bytes.len() as u64)
            && let Ok(mut existing) = self.storage.open_read(path)
        {
            let mut current = Vec::with_capacity(bytes.len());
            if existing.read_to_end(&mut current).is_ok() && current == bytes {
                return Ok(());
            }
        }
        let path = self.storage.root().join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::io(parent, source))?;
        }
        let mut output = AtomicWriteFile::open(&path).map_err(|source| Error::io(&path, source))?;
        output
            .write_all(bytes)
            .map_err(|source| Error::io(&path, source))?;
        output.flush().map_err(|source| Error::io(&path, source))?;
        output.commit().map_err(|source| Error::io(&path, source))
    }

    fn write_save(&self, product: DreamcastProductId, save: &Save<'_>) -> Result<(), Error> {
        let (vmi_path, vms_path) = Self::save_paths(product, save);
        self.write_if_changed(&vms_path, save.vms())?;
        self.write_if_changed(&vmi_path, save.vmi().as_bytes())
    }

    fn describe_save(
        &self,
        product: DreamcastProductId,
        save: &Save<'_>,
    ) -> Result<SaveDescriptor, Error> {
        let header = save.header()?;
        let title = match header.long_description() {
            title if !title.is_empty() => title,
            _ => header.short_description(),
        };
        let (vmi_path, _) = Self::save_paths(product, save);
        let timestamp = self
            .storage
            .metadata(&vmi_path)
            .map(|metadata| metadata.mtime)
            .unwrap_or_else(|_| Timestamp::now());
        Ok(SaveDescriptor {
            id: product.into(),
            banner: header.icon(),
            title: title.into_owned(),
            description: String::from_utf8_lossy(header.application_id())
                .trim_end_matches(['\0', ' '])
                .to_owned(),
            timestamp,
            root_path: Self::product_path(product),
        })
    }

    fn snapshot(&self, selected: &[SaveDepotId]) -> Vec<([u8; 12], [Option<(u64, Timestamp)>; 2])> {
        let selected = selected.iter().map(|id| id.0).collect::<HashSet<_>>();
        self.saves()
            .into_iter()
            .filter(|(product, _)| {
                selected.is_empty() || selected.contains(&SaveDepotId::from(*product).0)
            })
            .map(|(product, save)| {
                let (vmi_path, vms_path) = Self::save_paths(product, &save);
                let metadata = |path| {
                    self.storage
                        .metadata(path)
                        .ok()
                        .map(|metadata| (metadata.len, metadata.mtime))
                };
                (save.identity(), [metadata(&vmi_path), metadata(&vms_path)])
            })
            .collect()
    }
}

impl From<vmi::ParseError> for Error {
    fn from(error: vmi::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "Dreamcast VMI".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl From<vms::ParseError> for Error {
    fn from(error: vms::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "Dreamcast VMS".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl From<raw::card::ParseError> for Error {
    fn from(error: raw::card::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "Dreamcast VMU".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl fmt::Debug for DreamcastMemoryCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DreamcastMemoryCard")
            .field("storage", &self.storage)
            .finish()
    }
}

impl MemoryCard for DreamcastMemoryCard {
    fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    fn reconcile(&self, primary: &[SaveDepotId]) -> Result<crate::Reconcile, Error> {
        let (mounted_product, projected) = self.project(primary)?;
        let originals = projected
            .iter()
            .map(|(product, save)| (save.identity(), (*product, save.vmi().clone().into_owned())))
            .collect::<HashMap<_, _>>();
        let card = self.clone();
        Ok(Arc::new(move |bytes| {
            let image = raw::card::VmuMemory::new(bytes, raw::card::USER_BLOCKS)?;
            let saves = image.saves()?;
            card.reconcile(mounted_product, &originals, &saves)
        }))
    }

    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error> {
        if format != "raw" {
            return Err(Error::Unsupported(format!(
                "Dreamcast memory cards do not support the '{format}' format"
            )));
        }
        let (_, projected) = self.project(primary)?;
        let saves = projected
            .iter()
            .map(|(_, save)| save.clone())
            .collect::<Vec<_>>();
        let mut image = raw::card::VmuMemory::empty(raw::card::USER_BLOCKS)?;
        for save in &saves {
            image.append_save(Save::new(save.vmi().as_bytes(), save.vms())?)?;
        }
        MemoryCardView::new(
            Arc::new(MemoryCardStorage::<Self>::new(
                self.clone(),
                primary,
                image.into_owned(),
                "dreamcast-vmu",
                "Dreamcast VMU",
            )?),
            "",
        )
    }
}

impl SaveGameSink for DreamcastMemoryCard {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        DreamcastProductId::new(
            package
                .serial()
                .expect("a Dreamcast package must expose a product ID")
                .as_bytes(),
        )
        .expect("a Dreamcast package contains a valid ten-byte product ID")
        .into()
    }

    fn enumerate_saves(&self) -> SaveDescriptors<'_> {
        let mut seen = HashSet::new();
        Box::new(
            self.saves()
                .into_iter()
                .filter(|(product, _)| seen.insert(*product))
                .filter_map(|(product, save)| self.describe_save(product, &save).ok())
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }

    fn export_save(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        let product = DreamcastProductId::try_from(*id)
            .map_err(|_| Error::InvalidPath(format!("unknown save depot {:016x}", id.0)))?;
        let saves = self
            .saves()
            .into_iter()
            .filter(|(candidate, _)| *candidate == product)
            .map(|(_, save)| save)
            .collect::<Vec<_>>();
        let representative = saves
            .first()
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", id.0)))?;
        let save_descriptor = self.describe_save(product, representative)?;
        let mut blobs = Vec::with_capacity(saves.len() * 2);
        for save in saves {
            let (vmi_path, vms_path) = Self::save_paths(product, &save);
            for (target_path, bytes) in [(vmi_path, save.vmi().as_bytes()), (vms_path, save.vms())]
            {
                blobs.push(SaveBlob {
                    target_path: PathBuf::from(target_path),
                    blake3: *blake3::hash(bytes).as_bytes(),
                    bytes: bytes.to_vec(),
                });
            }
        }
        Ok(SaveDepot {
            save_descriptor,
            blobs,
        })
    }

    fn install_save<'a>(
        &'a self,
        blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let mut blobs = blobs;
            let mut product = None;
            let mut pairs = BTreeMap::<String, (Option<Vec<u8>>, Option<Vec<u8>>)>::new();
            while let Some(blob) = blobs.next().await {
                if *blake3::hash(&blob.bytes).as_bytes() != blob.blake3 {
                    return Err(Error::IntegrityMatchFailed {
                        failed: blob.target_path.to_string_lossy().into_owned(),
                    });
                }
                let path = blob.target_path.to_string_lossy().replace('\\', "/");
                let parts = path.split('/').collect::<Vec<_>>();
                if parts.len() != 3 || parts[0] != SAVE_ROOT {
                    return Err(Error::InvalidPath(path));
                }
                let candidate = DreamcastProductId::new(parts[1].as_bytes())
                    .ok_or_else(|| Error::InvalidPath(path.clone()))?;
                if product.is_some_and(|product| product != candidate) {
                    return Err(Error::Unsupported(
                        "a Dreamcast save install may contain only one product ID".to_owned(),
                    ));
                }
                product = Some(candidate);
                let (stem, extension) = parts[2]
                    .rsplit_once('.')
                    .ok_or_else(|| Error::InvalidPath(path.clone()))?;
                let pair = pairs.entry(stem.to_owned()).or_default();
                match extension.to_ascii_lowercase().as_str() {
                    "vmi" if pair.0.is_none() => pair.0 = Some(blob.bytes),
                    "vms" if pair.1.is_none() => pair.1 = Some(blob.bytes),
                    _ => return Err(Error::InvalidPath(path)),
                }
            }
            let product = product.ok_or(Error::InstallIncomplete)?;
            let mut representative = None;
            for (stem, (vmi, vms)) in pairs {
                let save = Save::new(
                    vmi.ok_or(Error::InstallIncomplete)?,
                    vms.ok_or(Error::InstallIncomplete)?,
                )?;
                if !save.vmi().resource_stem().eq_ignore_ascii_case(&stem) {
                    return Err(Error::InvalidPath(stem));
                }
                self.write_save(product, &save)?;
                representative.get_or_insert(save);
            }
            self.describe_save(product, &representative.ok_or(Error::InstallIncomplete)?)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let activity = self
            .storage
            .watch(&[SAVE_ROOT])?
            .ok_or_else(|| Error::Unsupported("Dreamcast storage cannot watch saves".to_owned()))?;
        let card = self.clone();
        let selected = saves.to_vec();
        Ok(SaveWatcher::with_fingerprint(
            selected.clone(),
            activity,
            Duration::from_millis(100),
            move || card.snapshot(&selected),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impls::memorycard::RawMemoryCardImage;

    fn fixture(file_name: [u8; 12], resource: [u8; 8], blocks: usize) -> Save<'static> {
        let mut vms = vec![0; blocks * vms::BLOCK_SIZE];
        vms[..16].copy_from_slice(b"SONIC ADVENTURE ");
        vms[16..48].copy_from_slice(b"Dreamcast storage test          ");
        vms[48..64].copy_from_slice(b"SONIC2          ");
        let vmi = Vmi::synthesize(
            resource,
            file_name,
            b"Sonic Adventure 2",
            b"SEGA",
            [0x20, 0x26, 0x08, 0x23, 0x12, 0, 0, 0],
            0,
            vms.len(),
        )
        .unwrap();
        Save::from_parts(vmi, vms).unwrap()
    }

    #[tokio::test]
    async fn canonical_saves_are_grouped_by_product_id_and_project_to_raw() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = DreamcastMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let product = DreamcastProductId::new(b"MK-51117").unwrap();
        let save = fixture(*b"SONIC2_SAVE\0", *b"SONIC2__", 2);
        card.write_save(product, &save).unwrap();

        assert!(card.storage.exists("card/MK-51117/SONIC2__.vmi"));
        assert!(card.storage.exists("card/MK-51117/SONIC2__.vms"));
        let view = card.view(&[product.into()], "raw").unwrap();
        let mut image = Vec::new();
        view.storage()
            .open_read("")
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        let raw = raw::card::VmuMemory::new(&image, raw::card::USER_BLOCKS).unwrap();
        assert_eq!(raw.saves().unwrap()[0].identity(), *b"SONIC2_SAVE\0");
    }

    #[tokio::test]
    async fn selection_keeps_whole_product_groups() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = DreamcastMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let primary = DreamcastProductId::new(b"T-1207N").unwrap();
        for (index, resource) in [*b"DISC1___", *b"DISC2___"].into_iter().enumerate() {
            let mut name = *b"CODEV_SAVE00";
            name[11] = b'0' + index as u8;
            card.write_save(primary, &fixture(name, resource, 30))
                .unwrap();
        }
        let (_, projected) = card.project(&[primary.into()]).unwrap();
        assert_eq!(projected.len(), 2);
        assert!(projected.iter().all(|(product, _)| *product == primary));
    }

    #[tokio::test]
    async fn selection_includes_savegroup_products_in_one_vmu() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = DreamcastMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let primary = DreamcastProductId::new(b"T-30004M").unwrap();
        let grouped = DreamcastProductId::new(b"T-30005M").unwrap();
        card.write_save(primary, &fixture(*b"D2_DISC1SAVE", *b"D2DISC1_", 1))
            .unwrap();
        card.write_save(grouped, &fixture(*b"D2_DISC2SAVE", *b"D2DISC2_", 1))
            .unwrap();

        let (_, projected) = card.project(&[primary.into()]).unwrap();
        assert_eq!(
            projected
                .iter()
                .map(|(product, _)| *product)
                .collect::<HashSet<_>>(),
            HashSet::from([primary, grouped])
        );

        let view = card.view(&[primary.into()], "raw").unwrap();
        let mut image = Vec::new();
        view.storage()
            .open_read("")
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        let raw = raw::card::VmuMemory::new(&image, raw::card::USER_BLOCKS).unwrap();
        assert_eq!(
            raw.saves()
                .unwrap()
                .iter()
                .map(Save::identity)
                .collect::<HashSet<_>>(),
            HashSet::from([*b"D2_DISC1SAVE", *b"D2_DISC2SAVE"])
        );
    }

    #[tokio::test]
    async fn new_raw_saves_are_assigned_to_the_primary_product() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = DreamcastMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let primary = DreamcastProductId::new(b"MK-51117").unwrap();
        let view = card.view(&[primary.into()], "raw").unwrap();
        let mut image = raw::card::VmuMemory::empty(raw::card::USER_BLOCKS).unwrap();
        image
            .append_save(fixture(*b"NEWSAVE1DATA", *b"IGNORED_", 1))
            .unwrap();
        let image = image.into_owned();
        let output = view.storage().open("", true).unwrap();
        assert_eq!(output.write_at(0, &image).unwrap(), image.len());
        view.storage().sync("").unwrap();

        assert!(card.storage.exists("card/MK-51117/NEWSAVE1.vmi"));
        assert!(card.storage.exists("card/MK-51117/NEWSAVE1.vms"));
        assert_eq!(card.enumerate_saves().count(), 1);
    }
}
