mod dolphin;
pub mod gci;
mod raw;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use atomic_write_file::AtomicWriteFile;
use futures::{Stream, StreamExt};
use jiff::Timestamp;
use parking_lot::Mutex;
use uuid::Uuid;

use self::dolphin::DolphinMemoryStorage;
use self::gci::{Region, Save};
use self::raw::RawMemoryStorage;
use crate::impls::GameCubeDiscId;
use crate::savegame::{
    Package, SaveBlob, SaveDepot, SaveDepotId, SaveDescriptor, SaveGameSink, SaveWatcher,
};
use crate::savegroups::TARGET_CAPACITY_PERCENT;
use crate::{
    BoxFuture, ChangeSignal, DirectoryStorage, EntryKind, Error, InstallResult, MemoryCard,
    MemoryCardView, Metadata, MountableStorage, ReadDir, Storage, StorageFile, StorageRead,
    VolumeStorage,
};

const SAVE_ROOT: &str = "card";

#[derive(Clone)]
pub struct GameCubeMemoryCard {
    storage: Arc<DirectoryStorage>,
    id: Uuid,
}

impl GameCubeMemoryCard {
    pub const PLATFORM: &str = "NINTENDO_GCN";

    pub async fn new_empty(
        storage: DirectoryStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        storage.create_dir_all(SAVE_ROOT)?;
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Self { storage, id })
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
        Ok(Self {
            storage,
            id: identity.id,
        })
    }

    pub async fn metadata(&self) -> Result<crate::NandMetadata, Error> {
        self.storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)
    }

    pub async fn game_saved(&self) -> Result<(), Error> {
        self.watch_saves(&[])?.changed().await;
        Ok(())
    }

    fn saves(&self) -> Vec<Save<'static>> {
        let Ok(entries) = self.storage.read_dir(SAVE_ROOT, false) else {
            return Vec::new();
        };
        let mut saves = entries
            .filter(|entry| {
                entry.kind == EntryKind::File && entry.name.to_ascii_lowercase().ends_with(".gci")
            })
            .filter_map(|entry| {
                let mut bytes = Vec::new();
                self.storage
                    .open_read(&entry.path)
                    .ok()?
                    .read_to_end(&mut bytes)
                    .ok()?;
                Save::new(bytes).ok()
            })
            .collect::<Vec<_>>();
        saves.sort_by_key(Save::file_name);
        saves
    }

    fn projection_region(primary: &[SaveDepotId]) -> Result<Region, Error> {
        let mut region = None;
        for id in primary {
            let code = GameCubeDiscId::try_from(*id).map_err(|_| {
                Error::Unsupported("a GameCube primary must be a GameCube product code".to_owned())
            })?;
            let candidate = Region::from_country_code(code.as_bytes()[3]);
            if region.is_some_and(|region| region != candidate) {
                return Err(Error::Unsupported(
                    "a GameCube projection cannot mix primary regions".to_owned(),
                ));
            }
            region = Some(candidate);
        }
        region.ok_or_else(|| {
            Error::Unsupported("a GameCube projection requires a primary game".to_owned())
        })
    }

    fn project(&self, primary: &[SaveDepotId]) -> Result<(Region, Vec<Save<'static>>), Error> {
        let region = Self::projection_region(primary)?;
        // A GameCube disc's number is separate from its six-byte product code. All
        // saves belonging to every disc of the primary title therefore share the
        // same depot ID and are required together.
        let required_ids = primary.iter().map(|id| id.0).collect::<HashSet<_>>();
        let mut candidates = self
            .saves()
            .into_iter()
            // A raw card has one text encoding, so optional fill must never pull
            // saves from a different region into either projection format.
            .filter(|save| save.region() == region)
            .collect::<Vec<_>>();
        let mut identities = HashSet::new();
        if candidates
            .iter()
            .any(|save| !identities.insert(save.identity()))
        {
            return Err(Error::Unsupported(
                "canonical GameCube storage contains duplicate save identities".to_owned(),
            ));
        }
        candidates
            .sort_by_key(|save| (!required_ids.contains(&save.depot_id().0), save.file_name()));

        let mut projected = Vec::new();
        let mut blocks = 0u32;
        let capacity_blocks = u32::from(raw::card::MC251_BLOCKS);
        let target_blocks = capacity_blocks * TARGET_CAPACITY_PERCENT / 100;
        for save in candidates {
            let is_required = required_ids.contains(&save.depot_id().0);
            let save_blocks = u32::from(save.block_count());
            if projected.len() == raw::card::DIRECTORY_ENTRIES
                || blocks + save_blocks > capacity_blocks
            {
                if is_required {
                    return Err(Error::Unsupported(
                        "the required GameCube saves do not fit on an MC-251".to_owned(),
                    ));
                }
                continue;
            }
            if !is_required && blocks + save_blocks > target_blocks {
                continue;
            }
            blocks += save_blocks;
            projected.push(save);
        }
        Ok((region, projected))
    }

    fn reconcile(
        &self,
        region: Region,
        projected: &HashMap<Vec<u8>, String>,
        saves: &[Save<'_>],
    ) -> Result<(), Error> {
        let mut decoded = HashMap::new();
        let mut paths = HashSet::new();
        for save in saves {
            if save.region() != region {
                return Err(Error::Unsupported(
                    "a GameCube projection cannot contain saves from another region".to_owned(),
                ));
            }
            let path = format!("{SAVE_ROOT}/{}", save.file_name());
            if decoded.insert(save.identity(), path.clone()).is_some() {
                return Err(Error::Unsupported(
                    "a GameCube projection contains duplicate save identities".to_owned(),
                ));
            }
            if !paths.insert(path) {
                return Err(Error::Unsupported(
                    "a GameCube projection contains colliding GCI filenames".to_owned(),
                ));
            }
        }

        for save in saves {
            let path = decoded
                .get(&save.identity())
                .expect("decoded saves have canonical paths");
            self.write_if_changed(path, save.as_bytes())?;
        }
        for (identity, path) in projected {
            if decoded.get(identity) != Some(path) {
                self.storage.remove(path)?;
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
        let mut output = AtomicWriteFile::open(&path).map_err(|source| Error::io(&path, source))?;
        output
            .write_all(bytes)
            .map_err(|source| Error::io(&path, source))?;
        output.flush().map_err(|source| Error::io(&path, source))?;
        output.commit().map_err(|source| Error::io(&path, source))
    }

    fn describe_save(&self, save: &Save<'_>) -> Result<SaveDescriptor, Error> {
        let (title, description) = save
            .comments()
            .map(|(title, description)| (title.into_owned(), description.into_owned()))
            .unwrap_or_else(|| {
                (
                    String::from_utf8_lossy(save.console_name()).into_owned(),
                    String::from_utf8_lossy(save.game_code()).into_owned(),
                )
            });
        let path = format!("{SAVE_ROOT}/{}", save.file_name());
        let timestamp = self
            .storage
            .metadata(&path)
            .map(|metadata| metadata.mtime)
            .unwrap_or_else(|_| Timestamp::now());
        Ok(SaveDescriptor {
            id: save.depot_id(),
            banner: None,
            title,
            description,
            timestamp,
            root_path: path,
        })
    }

    fn snapshot(&self, saves: &[SaveDepotId]) -> Vec<(Vec<u8>, Option<(u64, Timestamp)>)> {
        let selected = saves.iter().map(|id| id.0).collect::<HashSet<_>>();
        self.saves()
            .into_iter()
            .filter(|save| selected.is_empty() || selected.contains(&save.depot_id().0))
            .map(|save| {
                let path = format!("{SAVE_ROOT}/{}", save.file_name());
                let metadata = self
                    .storage
                    .metadata(&path)
                    .ok()
                    .map(|metadata| (metadata.len, metadata.mtime));
                (save.identity(), metadata)
            })
            .collect()
    }
}

impl From<gci::ParseError> for Error {
    fn from(error: gci::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "GameCube GCI".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl From<raw::card::ParseError> for Error {
    fn from(error: raw::card::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "GameCube memory card".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl fmt::Debug for GameCubeMemoryCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GameCubeMemoryCard")
            .field("storage", &self.storage)
            .finish()
    }
}

impl Storage for GameCubeMemoryCard {
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
    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.storage.create(path)
    }
    fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.storage.open(path)
    }
    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        self.storage.open_read(path)
    }
    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.storage.append(path)
    }
    fn remove(&self, path: &str) -> Result<(), Error> {
        self.storage.remove(path)
    }
    fn watch(&self, paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        self.storage.watch(paths)
    }
    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        self.storage.label()
    }
    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        self.storage.set_label(label)
    }
    fn install_file<'a>(
        &'a self,
        source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        self.storage.install_file(source, dest)
    }
}

impl MemoryCard for GameCubeMemoryCard {
    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error> {
        let (region, projected) = self.project(primary)?;
        let projected_paths = projected
            .iter()
            .map(|save| (save.identity(), format!("{SAVE_ROOT}/{}", save.file_name())))
            .collect::<HashMap<_, _>>();
        let card = self.clone();
        let reconcile = move |saves: &[Save<'_>]| card.reconcile(region, &projected_paths, saves);
        let storage: Arc<dyn MountableStorage> = match format {
            "dolphin" => Arc::new(DolphinMemoryStorage::new(
                self.storage.clone(),
                &projected,
                reconcile,
            )),
            "raw" => Arc::new(RawMemoryStorage::new(
                self.storage.clone(),
                self.id,
                region,
                u32::from(raw::card::MC251_BLOCKS),
                &projected,
                reconcile,
            )?),
            _ => {
                return Err(Error::Unsupported(format!(
                    "GameCube memory cards do not support the '{format}' format"
                )));
            }
        };
        MemoryCardView::new(storage, "")
    }
}

impl SaveGameSink for GameCubeMemoryCard {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let serial = package
            .serial()
            .expect("a GameCube package must expose a product code");
        GameCubeDiscId::new(serial.trim().as_bytes())
            .expect("a GameCube product code contains six ASCII letters or digits")
            .into()
    }

    fn enumerate_saves(&self) -> impl Iterator<Item = SaveDescriptor> + '_ {
        let mut seen = HashSet::new();
        self.saves()
            .into_iter()
            .filter(|save| seen.insert(save.depot_id().0))
            .filter_map(|save| self.describe_save(&save).ok())
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn export_save(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        let saves = self
            .saves()
            .into_iter()
            .filter(|save| save.depot_id() == *id)
            .collect::<Vec<_>>();
        let representative = saves
            .first()
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", id.0)))?;
        let save_descriptor = self.describe_save(representative)?;
        let blobs = saves
            .into_iter()
            .map(|save| SaveBlob {
                target_path: PathBuf::from(format!("{SAVE_ROOT}/{}", save.file_name())),
                blake3: *blake3::hash(save.as_bytes()).as_bytes(),
                bytes: save.as_bytes().to_vec(),
            })
            .collect();
        Ok(SaveDepot {
            save_descriptor,
            blobs,
        })
    }

    fn install_save<'a>(
        &'a self,
        blobs: impl Stream<Item = SaveBlob> + Send + 'a,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let mut blobs = std::pin::pin!(blobs);
            let mut installed = None;
            let mut representative = None;
            while let Some(blob) = blobs.next().await {
                if *blake3::hash(&blob.bytes).as_bytes() != blob.blake3 {
                    return Err(Error::IntegrityMatchFailed {
                        failed: blob.target_path.to_string_lossy().into_owned(),
                    });
                }
                let save = Save::new(blob.bytes)?;
                if installed.is_some_and(|id| id != save.depot_id()) {
                    return Err(Error::Unsupported(
                        "a GameCube save install may contain only one depot".to_owned(),
                    ));
                }
                let path = format!("{SAVE_ROOT}/{}", save.file_name());
                self.write_if_changed(&path, save.as_bytes())?;
                installed = Some(save.depot_id());
                representative.get_or_insert(save);
            }
            self.describe_save(&representative.ok_or(Error::InstallIncomplete)?)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let activity = self
            .storage
            .watch(&[SAVE_ROOT])?
            .ok_or_else(|| Error::Unsupported("GameCube storage cannot watch saves".to_owned()))?;
        let signal = SaveSignal {
            card: self.clone(),
            saves: saves.to_vec(),
            activity,
            last: Mutex::new(self.snapshot(saves)),
        };
        Ok(SaveWatcher::new(saves.to_vec(), Box::new(signal)))
    }
}

struct SaveSignal {
    card: GameCubeMemoryCard,
    saves: Vec<SaveDepotId>,
    activity: Box<dyn ChangeSignal>,
    last: Mutex<Vec<(Vec<u8>, Option<(u64, Timestamp)>)>>,
}

impl ChangeSignal for SaveSignal {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                self.activity.changed().await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                let snapshot = self.card.snapshot(&self.saves);
                let mut last = self.last.lock();
                if *last != snapshot {
                    *last = snapshot;
                    return;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};

    use crate::impls::memorycard::RawMemoryCardImage;

    fn store(card: &GameCubeMemoryCard, save: &Save<'_>) {
        card.write_if_changed(
            &format!("{SAVE_ROOT}/{}", save.file_name()),
            save.as_bytes(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn projections_keep_regions_strictly_separate() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = GameCubeMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        for save in [
            gci::fixture(b"GALE01", b"USA", 1),
            gci::fixture(b"GALP01", b"PAL", 1),
            gci::fixture(b"GALJ01", b"JPN", 1),
        ] {
            store(&card, &save);
        }
        let primary: SaveDepotId = GameCubeDiscId::new(b"GALE01").unwrap().into();

        let dolphin = card.view(&[primary], "dolphin").unwrap();
        let entries = dolphin
            .storage()
            .read_dir("", false)
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].name.contains("GALE"));

        let raw = card.view(&[primary], "raw").unwrap();
        let mut image = Vec::new();
        raw.storage()
            .open_read("")
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        let parsed =
            raw::card::MemoryCard::new(&image, u32::from(raw::card::MC251_BLOCKS)).unwrap();
        let saves = parsed.saves().unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].region(), Region::Usa);
    }

    #[tokio::test]
    async fn dolphin_writes_are_visible_in_raw_projection() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = GameCubeMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let primary: SaveDepotId = GameCubeDiscId::new(b"GALE01").unwrap().into();
        let dolphin = card.view(&[primary], "dolphin").unwrap();
        let save = gci::fixture(b"GALE01", b"Created", 1);
        let mut file = dolphin.storage().create("01-GALE-Created.gci").unwrap();
        file.write_all(save.as_bytes()).unwrap();
        file.flush().unwrap();

        let raw = card.view(&[primary], "raw").unwrap();
        let mut image = Vec::new();
        raw.storage()
            .open_read("")
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        let parsed =
            raw::card::MemoryCard::new(&image, u32::from(raw::card::MC251_BLOCKS)).unwrap();
        let saves = parsed.saves().unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].console_name(), b"Created");
    }

    #[tokio::test]
    async fn selection_keeps_multidisc_saves_and_twenty_percent_free() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = GameCubeMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let disc_one = gci::fixture(b"GALE01", b"DiscOne", 120);
        let disc_two = gci::fixture(b"GALE01", b"DiscTwo", 120);
        store(&card, &disc_one);
        store(&card, &disc_two);
        for index in 0..120u16 {
            let code = format!("G{index:02X}E01");
            let name = format!("Other{index:03}");
            let save = gci::fixture(code.as_bytes().try_into().unwrap(), name.as_bytes(), 2);
            store(&card, &save);
        }
        let primary_id = disc_one.depot_id();
        let (_, projected) = card.project(&[primary_id]).unwrap();
        assert_eq!(projected.len(), 2);
        assert!(projected.iter().all(|save| save.depot_id() == primary_id));

        for save in [disc_one, disc_two] {
            card.storage
                .remove(&format!("{SAVE_ROOT}/{}", save.file_name()))
                .unwrap();
        }
        let primary_id: SaveDepotId = GameCubeDiscId::new(b"GALE01").unwrap().into();
        let (_, projected) = card.project(&[primary_id]).unwrap();
        assert_eq!(projected.len(), 100);
        assert_eq!(
            projected
                .iter()
                .map(|save| u32::from(save.block_count()))
                .sum::<u32>(),
            u32::from(raw::card::MC251_BLOCKS) * TARGET_CAPACITY_PERCENT / 100
        );
    }

    #[tokio::test]
    async fn raw_writes_are_visible_in_dolphin_projection() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = GameCubeMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let original = gci::fixture(b"GALE01", b"Original", 1);
        store(&card, &original);
        let primary = original.depot_id();
        let raw = card.view(&[primary], "raw").unwrap();
        let replacement = gci::fixture(b"GALE01", b"Replacement", 2);
        let mut replacement_card =
            raw::card::MemoryCard::empty(u32::from(raw::card::MC251_BLOCKS)).unwrap();
        replacement_card
            .initialize_header(card.id, Region::Usa)
            .unwrap();
        replacement_card.append_save(replacement).unwrap();
        let image = replacement_card.into_owned();
        raw.storage().truncate("", image.len() as u64).unwrap();
        let mut output = raw.storage().open("").unwrap();
        output.seek(SeekFrom::Start(0)).unwrap();
        output.write_all(&image).unwrap();
        raw.storage().sync("").unwrap();

        let dolphin = card.view(&[primary], "dolphin").unwrap();
        let entries = dolphin
            .storage()
            .read_dir("", false)
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        let mut bytes = Vec::new();
        dolphin
            .storage()
            .open_read(&entries[0].path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(Save::new(bytes).unwrap().console_name(), b"Replacement");
    }

    #[tokio::test]
    async fn projections_reject_cross_region_writes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = GameCubeMemoryCard::new_empty(storage, Uuid::nil(), None)
            .await
            .unwrap();
        let primary: SaveDepotId = GameCubeDiscId::new(b"GALE01").unwrap().into();
        let raw = card.view(&[primary], "raw").unwrap();
        let pal = gci::fixture(b"GALP01", b"PAL", 1);
        let mut pal_card =
            raw::card::MemoryCard::empty(u32::from(raw::card::MC251_BLOCKS)).unwrap();
        pal_card.initialize_header(card.id, Region::Pal).unwrap();
        pal_card.append_save(pal).unwrap();
        let image = pal_card.into_owned();
        raw.storage().truncate("", image.len() as u64).unwrap();
        let mut output = raw.storage().open("").unwrap();
        output.write_all(&image).unwrap();
        assert!(raw.storage().sync("").is_err());
        assert!(card.saves().is_empty());
    }
}
