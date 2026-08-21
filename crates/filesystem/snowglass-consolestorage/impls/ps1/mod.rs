mod raw;

use std::collections::HashSet;
use std::fmt;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{Stream, StreamExt};
use image::RgbaImage;
use jiff::Timestamp;
use psxmem::{BAState, Block, DataBlock, MemCard};
use tempfile::NamedTempFile;
use uuid::Uuid;

use self::raw::{ENTRY as RAW_ENTRY, RawMemoryStorage};
use crate::savegame::{
    SaveBlob, SaveDepot, SaveDepotId, SaveDescriptor, SaveGameSink, SaveWatcher,
};
use crate::{
    BoxFuture, ChangeSignal, DirectoryStorage, Error, InstallResult, MemoryCard, MemoryCardView,
    Metadata, MountableStorage, ReadDir, Storage, StorageFile, StorageRead, VolumeStorage,
};

const BLOCK_SIZE: usize = 8 * 1024;
const FRAME_SIZE: usize = 128;
const DATA_BLOCKS: usize = 15;
const RAW_SIZE: usize = BLOCK_SIZE * (DATA_BLOCKS + 1);
const NATIVE_ID_SIZE: usize = 21;
const SAVE_ROOT: &str = "save-depots";

#[derive(Clone, Debug, PartialEq, Eq)]
struct CanonicalSave {
    id: SaveDepotId,
    native_id: [u8; NATIVE_ID_SIZE],
}

impl CanonicalSave {
    fn from_native_id(native_id: [u8; NATIVE_ID_SIZE]) -> Self {
        let digest = blake3::hash(&native_id);
        Self {
            id: SaveDepotId(u64::from_le_bytes(
                digest.as_bytes()[..8].try_into().expect("eight bytes"),
            )),
            native_id,
        }
    }

    fn from_directory_name(name: &str) -> Option<Self> {
        let bytes = decode_hex(name)?;
        let native_id = bytes.try_into().ok()?;
        Some(Self::from_native_id(native_id))
    }

    fn root(&self) -> String {
        format!("{SAVE_ROOT}/{}", encode_hex(&self.native_id))
    }

    fn data_path(&self) -> String {
        format!("{}/data.bin", self.root())
    }
}

#[derive(Clone)]
pub struct PlaystationMemoryCard {
    storage: Arc<DirectoryStorage>,
}

impl PlaystationMemoryCard {
    pub const PLATFORM: &str = "SONY_PSX";

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
                "`{:?}` is a {} volume",
                storage, identity.platform
            )));
        }
        storage.create_dir_all(SAVE_ROOT)?;
        Ok(Self { storage })
    }

    pub async fn metadata(&self) -> Result<crate::NandMetadata, Error> {
        self.storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)
    }

    pub async fn game_saved(&self) -> Result<(), Error> {
        self.watch_saves(&[])?.changed().await;
        Ok(())
    }

    fn saves(&self) -> Vec<CanonicalSave> {
        let Ok(entries) = self.storage.read_dir(SAVE_ROOT, false) else {
            return Vec::new();
        };
        let mut saves = entries
            .filter_map(|entry| CanonicalSave::from_directory_name(&entry.name))
            .filter(|save| self.storage.exists(&save.data_path()))
            .collect::<Vec<_>>();
        saves.sort_by(|left, right| left.native_id.cmp(&right.native_id));
        saves
    }

    fn save(&self, id: SaveDepotId) -> Option<CanonicalSave> {
        self.saves().into_iter().find(|save| save.id == id)
    }

    fn read_payload(&self, save: &CanonicalSave) -> Result<Vec<u8>, Error> {
        let path = save.data_path();
        let mut payload = Vec::new();
        self.storage
            .open_read(&path)?
            .read_to_end(&mut payload)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        Ok(payload)
    }

    fn describe_save(&self, save: &CanonicalSave) -> Result<SaveDescriptor, Error> {
        let (title, banner) = describe_payload(&self.read_payload(save)?)?;
        let title = if title.is_empty() {
            native_name(&save.native_id)
        } else {
            title
        };
        let timestamp = self
            .storage
            .metadata(&save.data_path())
            .map(|metadata| metadata.mtime)
            .unwrap_or_else(|_| Timestamp::now());
        Ok(SaveDescriptor {
            id: save.id,
            banner,
            title,
            description: native_name(&save.native_id),
            timestamp,
            root_path: save.root(),
        })
    }

    fn projection_order(&self, primary: &[SaveDepotId]) -> Result<Vec<CanonicalSave>, Error> {
        let saves = self.saves();
        let mut selected = HashSet::new();
        let mut ordered = Vec::new();
        for id in primary {
            if !selected.insert(id.0) {
                continue;
            }
            ordered.push(
                saves
                    .iter()
                    .find(|save| save.id == *id)
                    .cloned()
                    .ok_or_else(|| {
                        Error::InvalidPath(format!("unknown primary save depot {:016x}", id.0))
                    })?,
            );
        }
        ordered.extend(saves.into_iter().filter(|save| selected.insert(save.id.0)));
        Ok(ordered)
    }

    fn project(
        &self,
        primary: &[SaveDepotId],
        capacity_blocks: u32,
    ) -> Result<(Vec<u8>, Vec<CanonicalSave>), Error> {
        if capacity_blocks == 0 || capacity_blocks > DATA_BLOCKS as u32 {
            return Err(Error::Unsupported(format!(
                "PlayStation cards support 1 through {DATA_BLOCKS} data blocks"
            )));
        }

        let primary_ids: HashSet<u64> = primary.iter().map(|id| id.0).collect();
        let mut image = formatted_card();
        let mut projected = Vec::new();
        let mut next_block = 0usize;
        for save in self.projection_order(primary)? {
            let payload = self.read_payload(&save)?;
            let blocks = payload.len().max(1).div_ceil(BLOCK_SIZE);
            if next_block + blocks > capacity_blocks as usize {
                if primary_ids.contains(&save.id.0) {
                    return Err(Error::Unsupported(format!(
                        "primary saves require more than {capacity_blocks} PlayStation blocks"
                    )));
                }
                continue;
            }
            write_save(&mut image, next_block, &save.native_id, &payload, blocks);
            next_block += blocks;
            projected.push(save);
        }
        parse_card(&image)?;
        Ok((image, projected))
    }

    fn reconcile(&self, projected: &[CanonicalSave], image: &[u8]) -> Result<(), Error> {
        let parsed = parse_card(image)?;
        let decoded = decode_saves(image, &parsed)?;
        let decoded_names: HashSet<[u8; NATIVE_ID_SIZE]> =
            decoded.iter().map(|save| save.save.native_id).collect();

        for decoded in &decoded {
            self.write_if_changed(&decoded.save, &decoded.payload)?;
        }
        for save in projected {
            if decoded_names.contains(&save.native_id) {
                continue;
            }
            self.storage.remove(&save.data_path())?;
            self.storage.remove(&save.root())?;
        }
        Ok(())
    }

    fn write_if_changed(&self, save: &CanonicalSave, payload: &[u8]) -> Result<(), Error> {
        if self
            .read_payload(save)
            .is_ok_and(|existing| existing == payload)
        {
            return Ok(());
        }
        let root = save.root();
        let path = save.data_path();
        self.storage.create_dir_all(&root)?;
        let mut output = self.storage.create(&path)?;
        output.write_all(payload).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        output.flush().map_err(|source| Error::Io { path, source })
    }

    fn snapshot(&self, saves: &[SaveDepotId]) -> Vec<(u64, Option<(u64, Timestamp)>)> {
        let selected = if saves.is_empty() {
            self.saves()
        } else {
            saves.iter().filter_map(|id| self.save(*id)).collect()
        };
        selected
            .into_iter()
            .map(|save| {
                let metadata = self
                    .storage
                    .metadata(&save.data_path())
                    .ok()
                    .map(|metadata| (metadata.len, metadata.mtime));
                (save.id.0, metadata)
            })
            .collect()
    }
}

impl fmt::Debug for PlaystationMemoryCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaystationMemoryCard")
            .field("storage", &self.storage)
            .finish()
    }
}

impl Storage for PlaystationMemoryCard {
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

impl MemoryCard for PlaystationMemoryCard {
    fn block_size(&self) -> u64 {
        BLOCK_SIZE as u64
    }

    fn max_blocks(&self) -> u32 {
        DATA_BLOCKS as u32
    }

    fn view(
        &self,
        primary: &[SaveDepotId],
        capacity_blocks: u32,
        format: &str,
    ) -> Result<MemoryCardView, Error> {
        if format != "raw" {
            return Err(Error::Unsupported(format!(
                "PlayStation memory cards do not support the '{format}' format"
            )));
        }
        let (image, projected) = self.project(primary, capacity_blocks)?;
        let card = self.clone();
        let storage: Arc<dyn MountableStorage> = Arc::new(RawMemoryStorage::new(
            self.storage.clone(),
            image,
            move |bytes| card.reconcile(&projected, bytes),
        ));
        MemoryCardView::new(storage, RAW_ENTRY)
    }
}

impl SaveGameSink for PlaystationMemoryCard {
    fn enumerate_saves(&self) -> impl Iterator<Item = SaveDescriptor> + '_ {
        self.saves()
            .into_iter()
            .filter_map(|save| self.describe_save(&save).ok())
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn export_save(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        let save = self
            .save(*id)
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", id.0)))?;
        let bytes = self.read_payload(&save)?;
        Ok(SaveDepot {
            save_descriptor: self.describe_save(&save)?,
            blobs: vec![SaveBlob {
                target_path: PathBuf::from(save.data_path()),
                blake3: *blake3::hash(&bytes).as_bytes(),
                bytes,
            }],
        })
    }

    fn install_save<'a>(
        &'a self,
        blobs: impl Stream<Item = SaveBlob> + Send + 'a,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let mut blobs = std::pin::pin!(blobs);
            let mut installed = None;
            while let Some(blob) = blobs.next().await {
                let target = blob.target_path.to_string_lossy().replace('\\', "/");
                let save = save_from_data_path(&target)
                    .ok_or_else(|| Error::InvalidPath(target.clone()))?;
                if installed
                    .as_ref()
                    .is_some_and(|installed: &CanonicalSave| installed != &save)
                {
                    return Err(Error::Unsupported(
                        "a PlayStation save install may contain only one depot".to_owned(),
                    ));
                }
                let written = self
                    .storage
                    .install_file(Box::new(Cursor::new(blob.bytes)), &save.data_path())
                    .await?;
                if written.blake3 != blob.blake3 {
                    return Err(Error::IntegrityMatchFailed { failed: target });
                }
                installed = Some(save);
            }
            let save = installed.ok_or(Error::InstallIncomplete)?;
            self.describe_save(&save)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let activity = self.storage.watch(&[SAVE_ROOT])?.ok_or_else(|| {
            Error::Unsupported("PlayStation storage cannot watch saves".to_owned())
        })?;
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
    card: PlaystationMemoryCard,
    saves: Vec<SaveDepotId>,
    activity: Box<dyn ChangeSignal>,
    last: Mutex<Vec<(u64, Option<(u64, Timestamp)>)>>,
}

impl ChangeSignal for SaveSignal {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                self.activity.changed().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                let current = self.card.snapshot(&self.saves);
                let mut last = self.last.lock().expect("save watcher mutex poisoned");
                if *last != current {
                    *last = current;
                    return;
                }
            }
        })
    }
}

struct DecodedSave {
    save: CanonicalSave,
    payload: Vec<u8>,
}

fn decode_saves(image: &[u8], parsed: &MemCard) -> Result<Vec<DecodedSave>, Error> {
    let mut decoded = Vec::new();
    let mut claimed = HashSet::new();
    let mut names = HashSet::new();
    for (first, directory) in parsed.info.dir_frames.iter().enumerate() {
        if directory.state != BAState::AllocFirst as u32 {
            continue;
        }
        let chain = allocation_chain(parsed, first)?;
        if chain.iter().any(|block| !claimed.insert(*block)) {
            return Err(malformed("save allocation chains overlap"));
        }
        if !names.insert(directory.filename) {
            return Err(malformed("multiple saves have the same native identity"));
        }
        let mut payload = Vec::with_capacity(chain.len() * BLOCK_SIZE);
        for block in chain {
            let start = (block + 1) * BLOCK_SIZE;
            payload.extend_from_slice(&image[start..start + BLOCK_SIZE]);
        }
        decoded.push(DecodedSave {
            save: CanonicalSave::from_native_id(directory.filename),
            payload,
        });
    }
    for (block, directory) in parsed.info.dir_frames.iter().enumerate() {
        if matches!(
            directory.state,
            state if state == BAState::AllocFirst as u32
                || state == BAState::AllocMid as u32
                || state == BAState::AllocLast as u32
        ) && !claimed.contains(&block)
        {
            return Err(malformed("allocated block is not part of a save chain"));
        }
    }
    Ok(decoded)
}

fn allocation_chain(parsed: &MemCard, first: usize) -> Result<Vec<usize>, Error> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut block = first;
    loop {
        if block >= DATA_BLOCKS || !seen.insert(block) {
            return Err(malformed("save contains an invalid allocation chain"));
        }
        chain.push(block);
        let entry = &parsed.info.dir_frames[block];
        let next = entry.next_block;
        let expected = if block == first {
            BAState::AllocFirst as u32
        } else if next == u16::MAX {
            BAState::AllocLast as u32
        } else {
            BAState::AllocMid as u32
        };
        if entry.state != expected {
            return Err(malformed("save allocation chain has incompatible states"));
        }
        if next == u16::MAX {
            return Ok(chain);
        }
        block = usize::from(next);
    }
}

fn describe_payload(payload: &[u8]) -> Result<(String, Option<RgbaImage>), Error> {
    let data: [u8; BLOCK_SIZE] = payload
        .get(..BLOCK_SIZE)
        .ok_or_else(|| malformed("save payload is shorter than one block"))?
        .try_into()
        .expect("slice length checked");
    let parsed = std::panic::catch_unwind(|| DataBlock::load_data_block(Block { data }))
        .map_err(|_| malformed("psxmem panicked while decoding save data"))?
        .map_err(|error| malformed(&format!("psxmem rejected save data: {error}")))?;
    let title = parsed.title_frame.decode_title().unwrap_or_default();
    let banner = parsed.icon_frames.first().and_then(|icon| {
        let mut pixels = Vec::with_capacity(16 * 16 * 4);
        for byte in icon.data {
            for shift in [0, 4] {
                let color = parsed.title_frame.icon_palette[((byte >> shift) & 0x0f) as usize];
                pixels.extend_from_slice(&[
                    ((color & 0x1f) * 255 / 31) as u8,
                    (((color >> 5) & 0x1f) * 255 / 31) as u8,
                    (((color >> 10) & 0x1f) * 255 / 31) as u8,
                    if color == 0 { 0 } else { 255 },
                ]);
            }
        }
        RgbaImage::from_raw(16, 16, pixels)
    });
    Ok((title, banner))
}

fn write_save(
    image: &mut [u8],
    first_block: usize,
    native_id: &[u8; NATIVE_ID_SIZE],
    payload: &[u8],
    blocks: usize,
) {
    for chain_index in 0..blocks {
        let block = first_block + chain_index;
        let data_start = (block + 1) * BLOCK_SIZE;
        let source_start = chain_index * BLOCK_SIZE;
        let copied = payload.len().saturating_sub(source_start).min(BLOCK_SIZE);
        image[data_start..data_start + BLOCK_SIZE].fill(0xff);
        image[data_start..data_start + copied]
            .copy_from_slice(&payload[source_start..source_start + copied]);

        let frame_start = (block + 1) * FRAME_SIZE;
        let frame = &mut image[frame_start..frame_start + FRAME_SIZE];
        frame.fill(0);
        let state = if chain_index == 0 {
            BAState::AllocFirst as u32
        } else if chain_index + 1 == blocks {
            BAState::AllocLast as u32
        } else {
            BAState::AllocMid as u32
        };
        frame[..4].copy_from_slice(&state.to_le_bytes());
        if chain_index == 0 {
            frame[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            frame[10..31].copy_from_slice(native_id);
        }
        let next = if chain_index + 1 == blocks {
            u16::MAX
        } else {
            (block + 1) as u16
        };
        frame[8..10].copy_from_slice(&next.to_le_bytes());
        update_checksum(frame);
    }
}

fn parse_card(image: &[u8]) -> Result<MemCard, Error> {
    if image.len() != RAW_SIZE {
        return Err(malformed("raw image is not 128 KiB"));
    }
    if image[..2] != *b"MC" {
        return Err(malformed(
            "raw image does not have a PlayStation card header",
        ));
    }
    let mut file = NamedTempFile::new().map_err(|source| Error::Io {
        path: "temporary PlayStation card".to_owned(),
        source,
    })?;
    file.write_all(image).map_err(|source| Error::Io {
        path: file.path().display().to_string(),
        source,
    })?;
    file.flush().map_err(|source| Error::Io {
        path: file.path().display().to_string(),
        source,
    })?;
    let path = file.path().to_string_lossy().into_owned();
    std::panic::catch_unwind(|| MemCard::open(&path))
        .map_err(|_| malformed("psxmem panicked while decoding the raw image"))?
        .map_err(|error| malformed(&format!("psxmem rejected the raw image: {error}")))
}

fn formatted_card() -> Vec<u8> {
    let mut image = vec![0xff; RAW_SIZE];
    image[..BLOCK_SIZE].fill(0);
    image[0] = b'M';
    image[1] = b'C';
    for frame in 0..64 {
        update_checksum(&mut image[frame * FRAME_SIZE..(frame + 1) * FRAME_SIZE]);
    }
    for block in 0..DATA_BLOCKS {
        let start = (block + 1) * FRAME_SIZE;
        let frame = &mut image[start..start + FRAME_SIZE];
        frame.fill(0);
        frame[..4].copy_from_slice(&(BAState::Free as u32).to_le_bytes());
        frame[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        update_checksum(frame);
    }
    image
}

fn update_checksum(frame: &mut [u8]) {
    frame[FRAME_SIZE - 1] = frame[..FRAME_SIZE - 1]
        .iter()
        .fold(0, |checksum, byte| checksum ^ byte);
}

fn native_name(native_id: &[u8; NATIVE_ID_SIZE]) -> String {
    String::from_utf8_lossy(native_id)
        .trim_end_matches('\0')
        .to_owned()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(encoded: &str) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?))
        .collect()
}

fn hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn save_from_data_path(path: &str) -> Option<CanonicalSave> {
    let mut components = path.split('/');
    if components.next()? != SAVE_ROOT {
        return None;
    }
    let save = CanonicalSave::from_directory_name(components.next()?)?;
    (components.next()? == "data.bin" && components.next().is_none()).then_some(save)
}

fn malformed(reason: &str) -> Error {
    Error::MalformedMetadata {
        file: "PlayStation memory card".to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(name: &[u8]) -> [u8; NATIVE_ID_SIZE] {
        let mut native = [0; NATIVE_ID_SIZE];
        native[..name.len()].copy_from_slice(name);
        native
    }

    fn payload(marker: u8) -> Vec<u8> {
        let mut payload = vec![marker; BLOCK_SIZE];
        payload[..BLOCK_SIZE].fill(0);
        payload[0..2].copy_from_slice(b"SC");
        payload[2] = 0x11;
        payload[3] = 1;
        payload[4..6].copy_from_slice(&[0x82, 0x60]);
        payload[98..100].copy_from_slice(&0x001f_u16.to_le_bytes());
        payload[FRAME_SIZE..FRAME_SIZE * 2].fill(0x11);
        payload[FRAME_SIZE * 2] = marker;
        payload
    }

    fn add_save(card: &PlaystationMemoryCard, native_id: [u8; NATIVE_ID_SIZE], data: &[u8]) {
        let save = CanonicalSave::from_native_id(native_id);
        card.storage.create_dir_all(&save.root()).unwrap();
        card.storage
            .create(&save.data_path())
            .unwrap()
            .write_all(data)
            .unwrap();
    }

    #[test]
    fn psxmem_accepts_the_virtual_blank_card() {
        let image = formatted_card();
        assert_eq!(image.len(), RAW_SIZE);
        parse_card(&image).unwrap();
    }

    #[test]
    fn canonical_name_round_trips_native_identity() {
        let mut native_id = [0; NATIVE_ID_SIZE];
        native_id[..12].copy_from_slice(b"BASCUS-94900");
        let save = CanonicalSave::from_native_id(native_id);
        assert_eq!(
            CanonicalSave::from_directory_name(&encode_hex(&native_id)),
            Some(save)
        );
    }

    #[tokio::test]
    async fn a_new_card_materializes_only_in_its_raw_view() {
        let directory = tempfile::tempdir().unwrap();
        let card = PlaystationMemoryCard::new_empty(
            DirectoryStorage::new(directory.path()).await.unwrap(),
            Uuid::now_v7(),
            Some("Card One".to_owned()),
        )
        .await
        .unwrap();
        let view = card.view(&[], 15, "raw").unwrap();
        assert_eq!(
            view.storage().metadata(view.entry()).unwrap().len,
            RAW_SIZE as u64
        );
        assert!(!card.exists(RAW_ENTRY));
        view.storage().sync(view.entry()).unwrap();
        assert_eq!(card.enumerate_saves().count(), 0);
        assert_eq!(card.label().await.unwrap().as_deref(), Some("Card One"));
    }

    #[tokio::test]
    async fn primary_save_is_first_and_raw_deletions_reconcile_to_disk() {
        let directory = tempfile::tempdir().unwrap();
        let card = PlaystationMemoryCard::new_empty(
            DirectoryStorage::new(directory.path()).await.unwrap(),
            Uuid::now_v7(),
            None,
        )
        .await
        .unwrap();
        let first = CanonicalSave::from_native_id(native(b"BASCUS-00001"));
        let primary = CanonicalSave::from_native_id(native(b"BASCUS-00002"));
        add_save(&card, first.native_id, &payload(1));
        add_save(&card, primary.native_id, &payload(2));

        let view = card.view(&[primary.id], 2, "raw").unwrap();
        let mut image = Vec::new();
        view.storage()
            .open_read(view.entry())
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        let parsed = parse_card(&image).unwrap();
        assert_eq!(parsed.info.dir_frames[0].filename, primary.native_id);
        assert_eq!(parsed.info.dir_frames[1].filename, first.native_id);

        let watcher = card.watch_saves(&[primary.id]).unwrap();
        let mut replacement = formatted_card();
        write_save(&mut replacement, 0, &primary.native_id, &payload(3), 1);
        let mut output = view.storage().open(view.entry()).unwrap();
        output.write_all(&replacement).unwrap();
        view.storage().sync(view.entry()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), watcher.changed())
            .await
            .unwrap();

        assert!(!card.storage.exists(&first.data_path()));
        assert!(card.storage.exists(&primary.data_path()));
        let descriptor = card.describe_save(&primary).unwrap();
        assert_eq!(descriptor.title, "A");
        assert_eq!(descriptor.banner.unwrap().dimensions(), (16, 16));
    }
}
