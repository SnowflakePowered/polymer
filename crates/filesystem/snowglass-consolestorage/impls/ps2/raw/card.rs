use std::borrow::Cow;
use std::fmt;
use std::io::{Cursor, Write};

use encoding_rs::SHIFT_JIS;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{U16, U32, U64},
};

use super::super::psu::{self, Encoder, Save};
use crate::impls::RawMemoryCardImage;

const MAGIC: &[u8; 28] = b"Sony PS2 Memory Card Format ";
const HEADER_SIZE: usize = 0x154;
const FAT_ENTRY_SIZE: usize = size_of::<u32>();
const IFC_ENTRIES: usize = 32;
const DIRENT_SIZE: usize = 512;
const ECC_CHUNK_SIZE: usize = 128;
const ECC_CHUNK_SPARE_SIZE: usize = 4;
const ALLOCATED: u32 = 0x8000_0000;
const CHAIN_END: u32 = 0x7fff_ffff;
const FILE_MODE: u16 = 0x0010;
const DIRECTORY_MODE: u16 = 0x0020;
const USED_MODE: u16 = 0x8000;
pub(super) const PAGE_SIZE: usize = 512;
pub(super) const SPARE_SIZE: usize = 16;
pub(super) const RAW_PAGE_SIZE: usize = PAGE_SIZE + SPARE_SIZE;
pub(super) const ERASE_BLOCK_SIZE: usize = 8 * 1024;
pub(super) const MAX_BLOCKS: u32 = 64 * 1024 * 1024 / ERASE_BLOCK_SIZE as u32;
const PAGES_PER_CLUSTER: usize = 2;
const PAGES_PER_BLOCK: usize = 16;
const CLUSTER_SIZE: usize = PAGE_SIZE * PAGES_PER_CLUSTER;
const CLUSTERS_PER_BLOCK: usize = PAGES_PER_BLOCK / PAGES_PER_CLUSTER;
const ENTRIES_PER_CLUSTER: usize = CLUSTER_SIZE / FAT_ENTRY_SIZE;
const RESERVED_HEAD_CLUSTERS: usize = CLUSTERS_PER_BLOCK;
const RESERVED_BACKUP_BLOCKS: usize = 2;
const DEFAULT_DIRECTORY_MODE: u16 = 0x8427;
const DEFAULT_FILE_MODE: u16 = 0x8497;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RawSuperblock {
    magic: [u8; 28],
    version: [u8; 12],
    page_len: U16,
    pages_per_cluster: U16,
    pages_per_block: U16,
    unused: U16,
    clusters_per_card: U32,
    alloc_offset: U32,
    alloc_end: U32,
    rootdir_cluster: U32,
    backup_block1: U32,
    backup_block2: U32,
    padding: U64,
    ifc_list: [U32; IFC_ENTRIES],
    bad_block_list: [U32; 32],
    card_type: u8,
    card_flags: u8,
    trailing: [u8; 2],
}

const _: () = assert!(size_of::<RawSuperblock>() == HEADER_SIZE);

/// A validated, borrowed view of a PS2 memory-card superblock.
#[derive(Clone, Copy)]
pub(super) struct Superblock<'a> {
    raw: &'a RawSuperblock,
}

impl<'a> Superblock<'a> {
    pub(super) fn new(card: &'a [u8]) -> Result<Self, ParseError> {
        let header = card
            .get(..HEADER_SIZE)
            .ok_or(ParseError::TooShort(card.len()))?;
        let raw = RawSuperblock::ref_from_bytes(header).map_err(|_| ParseError::InvalidLayout)?;
        let superblock = Self { raw };
        superblock.validate()?;
        Ok(superblock)
    }

    pub(super) fn version(self) -> &'a str {
        let bytes = self.version_bytes();
        std::str::from_utf8(bytes).expect("the version is validated when the superblock is parsed")
    }

    pub(super) fn page_len(self) -> usize {
        usize::from(self.raw.page_len.get())
    }

    pub(super) fn pages_per_cluster(self) -> usize {
        usize::from(self.raw.pages_per_cluster.get())
    }

    pub(super) fn pages_per_block(self) -> usize {
        usize::from(self.raw.pages_per_block.get())
    }

    pub(super) fn clusters_per_card(self) -> usize {
        self.raw.clusters_per_card.get() as usize
    }

    pub(super) fn alloc_offset(self) -> usize {
        self.raw.alloc_offset.get() as usize
    }

    pub(super) fn alloc_end(self) -> usize {
        self.raw.alloc_end.get() as usize
    }

    pub(super) fn rootdir_cluster(self) -> usize {
        self.raw.rootdir_cluster.get() as usize
    }

    pub(super) fn backup_blocks(self) -> [usize; 2] {
        [
            self.raw.backup_block1.get() as usize,
            self.raw.backup_block2.get() as usize,
        ]
    }

    pub(super) fn card_type(self) -> u8 {
        self.raw.card_type
    }

    pub(super) fn card_flags(self) -> u8 {
        self.raw.card_flags
    }

    pub(super) fn cluster_size(self) -> usize {
        self.page_len() * self.pages_per_cluster()
    }

    pub(super) fn erase_block_size(self) -> usize {
        self.page_len() * self.pages_per_block()
    }

    pub(super) fn entries_per_cluster(self) -> usize {
        self.cluster_size() / FAT_ENTRY_SIZE
    }

    pub(super) fn indirect_fat_cluster(self, index: usize) -> Option<usize> {
        self.raw
            .ifc_list
            .get(index)
            .map(|cluster| cluster.get() as usize)
    }

    fn version_bytes(self) -> &'a [u8] {
        let end = self
            .raw
            .version
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.raw.version.len());
        &self.raw.version[..end]
    }

    fn required_ifc_entries(self) -> usize {
        let entries_per_cluster = self.entries_per_cluster();
        self.alloc_end()
            .div_ceil(entries_per_cluster)
            .div_ceil(entries_per_cluster)
    }

    fn validate(self) -> Result<(), ParseError> {
        if self.raw.magic != *MAGIC {
            return Err(ParseError::InvalidMagic);
        }
        if self.version_bytes().is_empty() || std::str::from_utf8(self.version_bytes()).is_err() {
            return Err(ParseError::InvalidVersion);
        }
        if self.page_len() == 0 {
            return Err(ParseError::InvalidGeometry("page length is zero"));
        }
        if !self.page_len().is_multiple_of(DIRENT_SIZE) {
            return Err(ParseError::InvalidGeometry(
                "page length is not divisible by a directory entry",
            ));
        }
        if !self.page_len().is_multiple_of(ECC_CHUNK_SIZE) {
            return Err(ParseError::InvalidGeometry(
                "page length is not divisible by an ECC chunk",
            ));
        }
        if self.pages_per_cluster() == 0 {
            return Err(ParseError::InvalidGeometry("pages per cluster is zero"));
        }
        if self.pages_per_block() == 0 {
            return Err(ParseError::InvalidGeometry("pages per block is zero"));
        }
        let cluster_size = self
            .page_len()
            .checked_mul(self.pages_per_cluster())
            .ok_or(ParseError::InvalidGeometry("cluster size overflows"))?;
        if !cluster_size.is_multiple_of(FAT_ENTRY_SIZE) {
            return Err(ParseError::InvalidGeometry(
                "cluster size is not divisible by a FAT entry",
            ));
        }
        if !self
            .pages_per_block()
            .is_multiple_of(self.pages_per_cluster())
        {
            return Err(ParseError::InvalidGeometry(
                "an erase block does not contain whole clusters",
            ));
        }
        if self.clusters_per_card() == 0 {
            return Err(ParseError::InvalidGeometry("card has no clusters"));
        }
        if self.alloc_offset() == 0 || self.alloc_offset() > self.clusters_per_card() {
            return Err(ParseError::InvalidGeometry(
                "allocation offset is outside the card",
            ));
        }
        if self.alloc_end() == 0
            || self
                .alloc_offset()
                .checked_add(self.alloc_end())
                .is_none_or(|end| end > self.clusters_per_card())
        {
            return Err(ParseError::InvalidGeometry(
                "allocatable area is outside the card",
            ));
        }
        if self.rootdir_cluster() != 0 {
            return Err(ParseError::InvalidRootDirectory(self.rootdir_cluster()));
        }

        let pages_per_card = self
            .clusters_per_card()
            .checked_mul(self.pages_per_cluster())
            .ok_or(ParseError::InvalidGeometry("card page count overflows"))?;
        if !pages_per_card.is_multiple_of(self.pages_per_block()) {
            return Err(ParseError::InvalidGeometry(
                "card does not contain whole erase blocks",
            ));
        }
        let blocks_per_card = pages_per_card / self.pages_per_block();
        let [backup1, backup2] = self.backup_blocks();
        if backup1 >= blocks_per_card || backup2 >= blocks_per_card || backup1 == backup2 {
            return Err(ParseError::InvalidBackupBlocks([backup1, backup2]));
        }
        if self.card_type() != 2 {
            return Err(ParseError::InvalidCardType(self.card_type()));
        }

        let required_ifc_entries = self.required_ifc_entries();
        if required_ifc_entries > IFC_ENTRIES {
            return Err(ParseError::TooManyIndirectFatClusters(required_ifc_entries));
        }
        for index in 0..required_ifc_entries {
            let cluster = self
                .indirect_fat_cluster(index)
                .expect("the required IFC count is bounded by the field length");
            if cluster == 0 || cluster >= self.alloc_offset() {
                return Err(ParseError::InvalidIndirectFatCluster { index, cluster });
            }
        }
        Ok(())
    }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct Dirent {
    mode: U16,
    unused: U16,
    length: U32,
    created: [u8; 8],
    cluster: U32,
    directory_entry: U32,
    modified: [u8; 8],
    attributes: U32,
    reserved: [u8; 28],
    name: [u8; 32],
    unused_tail: [u8; 416],
}

const _: () = assert!(size_of::<Dirent>() == DIRENT_SIZE);

impl Dirent {
    pub(super) fn mode(&self) -> u16 {
        self.mode.get()
    }

    pub(super) fn length(&self) -> usize {
        self.length.get() as usize
    }

    pub(super) fn cluster(&self) -> Option<usize> {
        (self.cluster.get() != u32::MAX).then_some(self.cluster.get() as usize)
    }

    pub(super) fn directory_entry(&self) -> usize {
        self.directory_entry.get() as usize
    }

    pub(super) fn created(&self) -> &[u8; 8] {
        &self.created
    }

    pub(super) fn modified(&self) -> &[u8; 8] {
        &self.modified
    }

    pub(super) fn attributes(&self) -> u32 {
        self.attributes.get()
    }

    pub(super) fn name(&self) -> Cow<'_, str> {
        let (name, _) = SHIFT_JIS.decode_without_bom_handling(self.name_bytes());
        name
    }

    pub(super) fn name_bytes(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.name.len());
        &self.name[..end]
    }

    pub(super) fn is_used(&self) -> bool {
        self.mode() & USED_MODE != 0
    }

    pub(super) fn is_file(&self) -> bool {
        self.mode() & FILE_MODE != 0
    }

    pub(super) fn is_directory(&self) -> bool {
        self.mode() & DIRECTORY_MODE != 0
    }
}

impl fmt::Debug for Dirent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Dirent")
            .field("mode", &format_args!("{:#06x}", self.mode()))
            .field("length", &self.length())
            .field("created", &self.created())
            .field("cluster", &self.cluster())
            .field("directory_entry", &self.directory_entry())
            .field("modified", &self.modified())
            .field("attributes", &self.attributes())
            .field("name", &self.name())
            .finish()
    }
}

/// A borrowed view over a plain or spare/ECC-bearing PS2 card image.
#[derive(Debug)]
pub(in crate::impls::ps2) struct MemoryCard<'a> {
    bytes: CardBytes<'a>,
    page_stride: usize,
    free_clusters: Vec<usize>,
}

#[derive(Debug)]
enum CardBytes<'a> {
    Borrowed(&'a [u8]),
    BorrowedMut(&'a mut [u8]),
    Owned(Vec<u8>),
}

impl CardBytes<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::BorrowedMut(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }

    fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::Borrowed(_) => None,
            Self::BorrowedMut(bytes) => Some(bytes),
            Self::Owned(bytes) => Some(bytes),
        }
    }
}

impl<'a> MemoryCard<'a> {
    fn validate(bytes: &[u8], blocks: u32) -> Result<usize, ParseError> {
        if blocks > MAX_BLOCKS {
            return Err(ParseError::InvalidGeometry(
                "card exceeds the maximum supported capacity",
            ));
        }
        let geometry = Geometry::new(blocks)?;
        let superblock = Superblock::new(bytes)?;
        if superblock.clusters_per_card() != geometry.clusters_per_card as usize {
            return Err(ParseError::InvalidGeometry(
                "superblock capacity does not match the requested block count",
            ));
        }
        let data_size = superblock
            .clusters_per_card()
            .checked_mul(superblock.cluster_size())
            .ok_or(ParseError::InvalidGeometry("card size overflows"))?;
        let page_count = superblock
            .clusters_per_card()
            .checked_mul(superblock.pages_per_cluster())
            .ok_or(ParseError::InvalidGeometry("card page count overflows"))?;
        let spare_size = superblock.page_len() / ECC_CHUNK_SIZE * ECC_CHUNK_SPARE_SIZE;
        let raw_page_size = superblock
            .page_len()
            .checked_add(spare_size)
            .ok_or(ParseError::InvalidGeometry("raw page size overflows"))?;
        let raw_size = page_count
            .checked_mul(raw_page_size)
            .ok_or(ParseError::InvalidGeometry("raw card size overflows"))?;
        let page_stride = if bytes.len() == data_size {
            superblock.page_len()
        } else if bytes.len() == raw_size {
            raw_page_size
        } else {
            return Err(ParseError::InvalidCardSize {
                actual: bytes.len(),
                data: data_size,
                raw: raw_size,
            });
        };
        Ok(page_stride)
    }

    pub(super) fn superblock(&self) -> Superblock<'_> {
        Superblock::new(self.bytes.as_slice()).expect("a memory card is validated when constructed")
    }

    #[cfg(test)]
    pub(super) fn had_spare(&self) -> bool {
        self.page_stride != self.superblock().page_len()
    }

    fn scan_free_clusters(&self) -> Result<Vec<usize>, ParseError> {
        (0..self.superblock().alloc_end())
            .filter_map(|cluster| match self.fat().entry(cluster) {
                Ok(FatEntry::Free) => Some(Ok(cluster)),
                Ok(FatEntry::End | FatEntry::Next(_)) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    pub(super) fn fat(&self) -> Fat<'_, 'a> {
        Fat { card: self }
    }

    pub(super) fn root_directory(&self) -> Result<Directory<'_, 'a>, ParseError> {
        let start = self.superblock().rootdir_cluster();
        let dot = self.dirent(start, 0)?;
        if !dot.is_used() || !dot.is_directory() || dot.name_bytes() != b"." {
            return Err(ParseError::InvalidDirectory(
                "first entry is not the used `.` directory entry",
            ));
        }
        self.directory_at(start, dot.length())
    }

    pub(super) fn directory(&self, dirent: &Dirent) -> Result<Directory<'_, 'a>, ParseError> {
        if !dirent.is_used() || !dirent.is_directory() {
            return Err(ParseError::InvalidDirectory(
                "entry does not name a used directory",
            ));
        }
        let start = dirent
            .cluster()
            .ok_or(ParseError::InvalidDirectory("directory has no cluster"))?;
        self.directory_at(start, dirent.length())
    }

    fn directory_at(&self, start: usize, count: usize) -> Result<Directory<'_, 'a>, ParseError> {
        if count < 2 {
            return Err(ParseError::InvalidDirectory(
                "directory has fewer than `.` and `..` entries",
            ));
        }
        let byte_len = count
            .checked_mul(DIRENT_SIZE)
            .ok_or(ParseError::DirectoryTooLarge(count))?;
        let superblock = self.superblock();
        let cluster_count = byte_len.div_ceil(superblock.cluster_size());
        if cluster_count > superblock.alloc_end() {
            return Err(ParseError::DirectoryTooLarge(count));
        }

        let mut clusters = Vec::with_capacity(cluster_count);
        let mut seen = vec![false; superblock.alloc_end()];
        let mut cluster = start;
        for index in 0..cluster_count {
            let slot = seen
                .get_mut(cluster)
                .ok_or(ParseError::ClusterOutOfRange(cluster))?;
            if std::mem::replace(slot, true) {
                return Err(ParseError::ChainLoop(start));
            }
            clusters.push(cluster);
            let fat_entry = self.fat().entry(cluster)?;
            if index + 1 < cluster_count {
                cluster = match fat_entry {
                    FatEntry::Next(next) => next,
                    FatEntry::End => {
                        return Err(ParseError::ChainEndedEarly {
                            start,
                            required: cluster_count,
                        });
                    }
                    FatEntry::Free => return Err(ParseError::UnallocatedCluster(cluster)),
                };
            } else if fat_entry == FatEntry::Free {
                return Err(ParseError::UnallocatedCluster(cluster));
            }
        }

        let directory = Directory {
            card: self,
            clusters,
            count,
        };
        let mut entries = directory.entries();
        let dot = entries
            .next()
            .expect("a directory has at least two entries");
        let dotdot = entries
            .next()
            .expect("a directory has at least two entries");
        if !dot.is_used() || !dot.is_directory() || dot.name_bytes() != b"." {
            return Err(ParseError::InvalidDirectory(
                "first entry is not the used `.` directory entry",
            ));
        }
        if !dotdot.is_used() || !dotdot.is_directory() || dotdot.name_bytes() != b".." {
            return Err(ParseError::InvalidDirectory(
                "second entry is not the used `..` directory entry",
            ));
        }
        drop(entries);
        Ok(directory)
    }

    fn dirent(&self, cluster: usize, index: usize) -> Result<&Dirent, ParseError> {
        let superblock = self.superblock();
        if cluster >= superblock.alloc_end() {
            return Err(ParseError::ClusterOutOfRange(cluster));
        }
        let entries_per_cluster = superblock.cluster_size() / DIRENT_SIZE;
        if index >= entries_per_cluster {
            return Err(ParseError::DirentOutOfRange(index));
        }
        let absolute_cluster = superblock
            .alloc_offset()
            .checked_add(cluster)
            .ok_or(ParseError::InvalidCardOffset(cluster))?;
        let offset = absolute_cluster
            .checked_mul(superblock.cluster_size())
            .and_then(|offset| offset.checked_add(index * DIRENT_SIZE))
            .ok_or(ParseError::InvalidCardOffset(cluster))?;
        let bytes = self.data_slice(offset, DIRENT_SIZE)?;
        Dirent::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidLayout)
    }

    fn raw_cluster_u32(&self, cluster: usize, index: usize) -> Result<u32, ParseError> {
        let superblock = self.superblock();
        if cluster >= superblock.clusters_per_card() {
            return Err(ParseError::ClusterOutOfRange(cluster));
        }
        if index >= superblock.entries_per_cluster() {
            return Err(ParseError::FatEntryOutOfRange(index));
        }
        let offset = cluster
            .checked_mul(superblock.cluster_size())
            .and_then(|offset| offset.checked_add(index * FAT_ENTRY_SIZE))
            .ok_or(ParseError::InvalidCardOffset(cluster))?;
        let bytes = self.data_slice(offset, FAT_ENTRY_SIZE)?;
        let value = U32::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidLayout)?;
        Ok(value.get())
    }

    fn file_chunks(&self, dirent: &Dirent) -> Result<Vec<&[u8]>, ParseError> {
        let superblock = self.superblock();
        let length = dirent.length();
        if length == 0 {
            return Ok(Vec::new());
        }
        let start = dirent
            .cluster()
            .ok_or(ParseError::InvalidFile("non-empty file has no cluster"))?;
        let required_clusters = length.div_ceil(superblock.cluster_size());
        if required_clusters > superblock.alloc_end() {
            return Err(ParseError::InvalidFile(
                "file length exceeds allocation space",
            ));
        }

        let mut chunks = Vec::with_capacity(length.div_ceil(superblock.page_len()));
        let mut seen = vec![false; superblock.alloc_end()];
        let mut cluster = start;
        let mut remaining = length;
        for index in 0..required_clusters {
            let slot = seen
                .get_mut(cluster)
                .ok_or(ParseError::ClusterOutOfRange(cluster))?;
            if std::mem::replace(slot, true) {
                return Err(ParseError::ChainLoop(start));
            }

            let absolute_cluster = superblock
                .alloc_offset()
                .checked_add(cluster)
                .ok_or(ParseError::InvalidCardOffset(cluster))?;
            let cluster_offset = absolute_cluster
                .checked_mul(superblock.cluster_size())
                .ok_or(ParseError::InvalidCardOffset(cluster))?;
            for page in 0..superblock.pages_per_cluster() {
                if remaining == 0 {
                    break;
                }
                let take = remaining.min(superblock.page_len());
                chunks.push(self.data_slice(cluster_offset + page * superblock.page_len(), take)?);
                remaining -= take;
            }

            let fat_entry = self.fat().entry(cluster)?;
            if index + 1 < required_clusters {
                cluster = match fat_entry {
                    FatEntry::Next(next) => next,
                    FatEntry::End => {
                        return Err(ParseError::ChainEndedEarly {
                            start,
                            required: required_clusters,
                        });
                    }
                    FatEntry::Free => return Err(ParseError::UnallocatedCluster(cluster)),
                };
            } else if fat_entry == FatEntry::Free {
                return Err(ParseError::UnallocatedCluster(cluster));
            }
        }
        debug_assert_eq!(remaining, 0);
        Ok(chunks)
    }

    fn data_slice(&self, offset: usize, len: usize) -> Result<&[u8], ParseError> {
        let page_len = self.superblock().page_len();
        let page = offset / page_len;
        let within_page = offset % page_len;
        if len > page_len - within_page {
            return Err(ParseError::NonContiguousField { offset, len });
        }
        let physical = page
            .checked_mul(self.page_stride)
            .and_then(|offset| offset.checked_add(within_page))
            .ok_or(ParseError::InvalidCardOffset(offset))?;
        self.bytes
            .as_slice()
            .get(physical..physical + len)
            .ok_or(ParseError::InvalidCardOffset(offset))
    }
}

impl<'a> RawMemoryCardImage<'a> for MemoryCard<'a> {
    type Error = ParseError;
    type Save<'card> = Save<'card>;

    const BLOCK_SIZE: u64 = ERASE_BLOCK_SIZE as u64;
    const MAX_BLOCKS: u32 = MAX_BLOCKS;

    fn empty(blocks: u32) -> Result<Self, Self::Error> {
        let bytes = MemoryCard::<'static>::format(blocks, &[], true)?;
        let page_stride = Self::validate(&bytes, blocks)?;
        let mut card = Self {
            bytes: CardBytes::Owned(bytes),
            page_stride,
            free_clusters: Vec::new(),
        };
        card.free_clusters = card.scan_free_clusters()?;
        Ok(card)
    }

    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error> {
        let page_stride = Self::validate(bytes, blocks)?;
        let mut card = Self {
            bytes: CardBytes::Borrowed(bytes),
            page_stride,
            free_clusters: Vec::new(),
        };
        card.free_clusters = card.scan_free_clusters()?;
        Ok(card)
    }

    fn new_mut(bytes: &'a mut [u8], blocks: u32) -> Result<Self, Self::Error> {
        let page_stride = Self::validate(bytes, blocks)?;
        let mut card = Self {
            bytes: CardBytes::BorrowedMut(bytes),
            page_stride,
            free_clusters: Vec::new(),
        };
        card.free_clusters = card.scan_free_clusters()?;
        Ok(card)
    }

    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error> {
        let root = self.root_directory()?;
        let mut saves = Vec::new();
        for entry in root.entries() {
            if !entry.is_used()
                || !entry.is_directory()
                || matches!(entry.name_bytes(), b"." | b"..")
            {
                continue;
            }
            let directory = self.directory(entry)?;
            let files = directory
                .entries()
                .filter(|file| file.is_used() && !matches!(file.name_bytes(), b"." | b".."))
                .collect::<Vec<_>>();
            if files.iter().any(|file| !file.is_file()) {
                return Err(ParseError::InvalidDirectory(
                    "save directory contains a non-file entry",
                ));
            }

            let mut encoder = Encoder::new(entry.as_bytes(), files.len())?;
            for file in files {
                encoder.file(file.as_bytes(), file.length(), self.file_chunks(file)?)?;
            }
            saves.push(encoder.finish()?);
        }
        Ok(saves)
    }

    fn append_save(&mut self, save: Self::Save<'_>) -> Result<(), Self::Error> {
        if self.bytes.as_mut_slice().is_none() {
            return Err(ParseError::ReadOnly);
        }

        let superblock = self.superblock();
        let alloc_offset = superblock.alloc_offset();
        let cluster_size = superblock.cluster_size();
        let entries_per_cluster = cluster_size / DIRENT_SIZE;
        let root = self.root_directory()?;
        if root.entries().any(|entry| {
            entry.is_used() && entry.is_directory() && entry.name_bytes() == save.name_bytes()
        }) {
            return Err(ParseError::DuplicateSave(save.name().into_owned()));
        }

        let root_count = root.count;
        let mut root_clusters = root.clusters.clone();
        let root_cluster_count = (root_count + 1)
            .checked_mul(DIRENT_SIZE)
            .ok_or(ParseError::FileTooLarge)?
            .div_ceil(cluster_size);
        let root_extra = root_cluster_count - root_clusters.len();
        let files = save.files().collect::<Vec<_>>();
        let directory_count = files.len().checked_add(2).ok_or(ParseError::FileTooLarge)?;
        let directory_clusters = MemoryCard::<'static>::dirent_clusters(directory_count)?;
        let file_cluster_counts = files
            .iter()
            .map(|file| file.data().len().div_ceil(cluster_size))
            .collect::<Vec<_>>();
        let needed = root_extra
            .checked_add(directory_clusters)
            .and_then(|needed| {
                file_cluster_counts
                    .iter()
                    .try_fold(needed, |needed, count| needed.checked_add(*count))
            })
            .ok_or(ParseError::FileTooLarge)?;
        if needed > self.free_clusters.len() {
            return Err(ParseError::Full {
                needed,
                available: self.free_clusters.len(),
            });
        }

        let allocated = &self.free_clusters[..needed];
        let mut cursor = 0;
        let new_root_clusters = allocated[cursor..cursor + root_extra].to_vec();
        cursor += root_extra;
        let save_directory_clusters = allocated[cursor..cursor + directory_clusters].to_vec();
        cursor += directory_clusters;
        let mut file_clusters = Vec::with_capacity(files.len());
        for count in file_cluster_counts {
            file_clusters.push(allocated[cursor..cursor + count].to_vec());
            cursor += count;
        }
        debug_assert_eq!(cursor, needed);

        let old_root_last = *root_clusters
            .last()
            .expect("the validated root directory has a cluster");
        root_clusters.extend_from_slice(&new_root_clusters);
        let save_directory_start = save_directory_clusters[0];
        let mut directory = Vec::with_capacity(directory_count * DIRENT_SIZE);
        directory.extend_from_slice(&MemoryCard::<'static>::dot_dirent(
            save.raw_dirent(),
            b".",
            directory_count,
            save_directory_start,
        )?);
        directory.extend_from_slice(&MemoryCard::<'static>::dot_dirent(
            save.raw_dirent(),
            b"..",
            0,
            root_clusters[0],
        )?);
        for (file, clusters) in files.iter().zip(&file_clusters) {
            let first_cluster = clusters.first().copied().unwrap_or(u32::MAX as usize);
            directory.extend_from_slice(&MemoryCard::<'static>::encoded_dirent(
                file.raw_dirent(),
                file.name_bytes(),
                DEFAULT_FILE_MODE,
                file.data().len(),
                first_cluster,
            )?);
        }
        let root_entry = MemoryCard::<'static>::encoded_dirent(
            save.raw_dirent(),
            save.name_bytes(),
            DEFAULT_DIRECTORY_MODE,
            directory_count,
            save_directory_start,
        )?;

        let mut fat_updates = Vec::new();
        if let Some(first) = new_root_clusters.first() {
            fat_updates.push((old_root_last, ALLOCATED | *first as u32));
            Self::extend_fat_updates(&mut fat_updates, &new_root_clusters);
        }
        Self::extend_fat_updates(&mut fat_updates, &save_directory_clusters);
        for clusters in &file_clusters {
            Self::extend_fat_updates(&mut fat_updates, clusters);
        }
        let fat_writes = fat_updates
            .into_iter()
            .map(|(cluster, value)| Ok((self.fat_entry_offset(cluster)?, value)))
            .collect::<Result<Vec<_>, ParseError>>()?;

        drop(root);
        for (file, clusters) in files.iter().zip(&file_clusters) {
            self.write_chain(clusters, file.data(), alloc_offset, cluster_size)?;
        }
        self.write_chain(
            &save_directory_clusters,
            &directory,
            alloc_offset,
            cluster_size,
        )?;
        let zero_cluster = vec![0; cluster_size];
        for cluster in &new_root_clusters {
            self.write_data((alloc_offset + *cluster) * cluster_size, &zero_cluster)?;
        }
        for (offset, value) in fat_writes {
            self.write_data(offset, &value.to_le_bytes())?;
        }

        let root_entry_cluster = root_clusters[root_count / entries_per_cluster];
        let root_entry_offset = (alloc_offset + root_entry_cluster) * cluster_size
            + (root_count % entries_per_cluster) * DIRENT_SIZE;
        self.write_data(root_entry_offset, &root_entry)?;
        let root_length_offset = (alloc_offset + root_clusters[0]) * cluster_size + 4;
        let new_root_count =
            u32::try_from(root_count + 1).map_err(|_| ParseError::ValueTooLarge(root_count + 1))?;
        self.write_data(root_length_offset, &new_root_count.to_le_bytes())?;

        self.free_clusters.drain(..needed);
        Ok(())
    }
}

impl MemoryCard<'_> {
    fn extend_fat_updates(updates: &mut Vec<(usize, u32)>, clusters: &[usize]) {
        for (index, cluster) in clusters.iter().copied().enumerate() {
            let next = clusters
                .get(index + 1)
                .map_or(CHAIN_END, |next| *next as u32);
            updates.push((cluster, ALLOCATED | next));
        }
    }

    fn fat_entry_offset(&self, cluster: usize) -> Result<usize, ParseError> {
        let superblock = self.superblock();
        let entries_per_cluster = superblock.entries_per_cluster();
        let indirect_index = cluster / entries_per_cluster;
        let indirect_slot = indirect_index / entries_per_cluster;
        let indirect_cluster = superblock
            .indirect_fat_cluster(indirect_slot)
            .ok_or(ParseError::MissingIndirectFatCluster(indirect_slot))?;
        let fat_cluster =
            self.raw_cluster_u32(indirect_cluster, indirect_index % entries_per_cluster)? as usize;
        if fat_cluster == 0 || fat_cluster >= superblock.alloc_offset() {
            return Err(ParseError::InvalidFatCluster {
                index: indirect_index,
                cluster: fat_cluster,
            });
        }
        Ok(fat_cluster * superblock.cluster_size()
            + (cluster % entries_per_cluster) * FAT_ENTRY_SIZE)
    }

    fn write_chain(
        &mut self,
        clusters: &[usize],
        data: &[u8],
        alloc_offset: usize,
        cluster_size: usize,
    ) -> Result<(), ParseError> {
        let zero_cluster = vec![0; cluster_size];
        for (index, cluster) in clusters.iter().copied().enumerate() {
            let offset = (alloc_offset + cluster) * cluster_size;
            self.write_data(offset, &zero_cluster)?;
            let source = index * cluster_size;
            let length = data.len().saturating_sub(source).min(cluster_size);
            if length != 0 {
                self.write_data(offset, &data[source..source + length])?;
            }
        }
        Ok(())
    }

    fn write_data(&mut self, mut offset: usize, mut data: &[u8]) -> Result<(), ParseError> {
        let page_len = self.superblock().page_len();
        let page_stride = self.page_stride;
        let bytes = self.bytes.as_mut_slice().ok_or(ParseError::ReadOnly)?;
        while !data.is_empty() {
            let page = offset / page_len;
            let within_page = offset % page_len;
            let length = data.len().min(page_len - within_page);
            let physical = page
                .checked_mul(page_stride)
                .and_then(|offset| offset.checked_add(within_page))
                .ok_or(ParseError::InvalidCardOffset(offset))?;
            bytes
                .get_mut(physical..physical + length)
                .ok_or(ParseError::InvalidCardOffset(offset))?
                .copy_from_slice(&data[..length]);

            if page_stride != page_len {
                let page_offset = page * page_stride;
                let spare = MemoryCard::spare(&bytes[page_offset..page_offset + page_len]);
                bytes[page_offset + page_len..page_offset + page_stride].copy_from_slice(&spare);
            }
            offset += length;
            data = &data[length..];
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FatEntry {
    Free,
    End,
    Next(usize),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Fat<'card, 'image> {
    card: &'card MemoryCard<'image>,
}

impl Fat<'_, '_> {
    pub(super) fn entry(self, cluster: usize) -> Result<FatEntry, ParseError> {
        let superblock = self.card.superblock();
        if cluster >= superblock.alloc_end() {
            return Err(ParseError::ClusterOutOfRange(cluster));
        }
        let entries_per_cluster = superblock.entries_per_cluster();
        let indirect_index = cluster / entries_per_cluster;
        let indirect_slot = indirect_index / entries_per_cluster;
        let indirect_cluster = superblock
            .indirect_fat_cluster(indirect_slot)
            .ok_or(ParseError::MissingIndirectFatCluster(indirect_slot))?;
        let fat_cluster = self
            .card
            .raw_cluster_u32(indirect_cluster, indirect_index % entries_per_cluster)?
            as usize;
        if fat_cluster == 0 || fat_cluster >= superblock.alloc_offset() {
            return Err(ParseError::InvalidFatCluster {
                index: indirect_index,
                cluster: fat_cluster,
            });
        }
        let raw = self
            .card
            .raw_cluster_u32(fat_cluster, cluster % entries_per_cluster)?;
        if raw & ALLOCATED == 0 {
            return Ok(FatEntry::Free);
        }
        let next = (raw & !ALLOCATED) as usize;
        if next == CHAIN_END as usize {
            return Ok(FatEntry::End);
        }
        if next >= superblock.alloc_end() {
            return Err(ParseError::InvalidFatNext { cluster, next });
        }
        Ok(FatEntry::Next(next))
    }
}

#[derive(Debug)]
pub(super) struct Directory<'card, 'image> {
    card: &'card MemoryCard<'image>,
    clusters: Vec<usize>,
    count: usize,
}

impl Directory<'_, '_> {
    pub(super) fn entries(&self) -> impl ExactSizeIterator<Item = &Dirent> + '_ {
        let entries_per_cluster = self.card.superblock().cluster_size() / DIRENT_SIZE;
        (0..self.count).map(move |index| {
            let cluster = self.clusters[index / entries_per_cluster];
            self.card
                .dirent(cluster, index % entries_per_cluster)
                .expect("directory clusters and entry bounds are validated at construction")
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Geometry {
    blocks: u32,
    clusters_per_card: u32,
    alloc_offset: u32,
    alloc_end: u32,
    ifc_cluster: u32,
    fat_clusters: u32,
}

impl Geometry {
    fn new(capacity_blocks: u32) -> Result<Self, ParseError> {
        let blocks = capacity_blocks;
        let clusters_per_block = CLUSTERS_PER_BLOCK as u32;
        let entries_per_cluster = ENTRIES_PER_CLUSTER as u32;
        if blocks <= RESERVED_BACKUP_BLOCKS as u32 {
            return Err(ParseError::InvalidGeometry(
                "card has no room outside its backup blocks",
            ));
        }
        let clusters_per_card = blocks
            .checked_mul(clusters_per_block)
            .ok_or(ParseError::InvalidGeometry("card cluster count overflows"))?;
        let reserved_tail = RESERVED_BACKUP_BLOCKS as u32 * clusters_per_block;
        let ifc_cluster = RESERVED_HEAD_CLUSTERS as u32;
        let mut fat_clusters = 1;
        loop {
            let alloc_offset = ifc_cluster + 1 + fat_clusters;
            let alloc_end = clusters_per_card
                .checked_sub(alloc_offset + reserved_tail)
                .ok_or(ParseError::InvalidGeometry(
                    "card has no allocatable clusters",
                ))?;
            let needed = alloc_end.div_ceil(entries_per_cluster);
            if needed <= fat_clusters {
                if fat_clusters > entries_per_cluster {
                    return Err(ParseError::InvalidGeometry(
                        "card requires more than one indirect FAT cluster",
                    ));
                }
                return Ok(Self {
                    blocks,
                    clusters_per_card,
                    alloc_offset,
                    alloc_end,
                    ifc_cluster,
                    fat_clusters,
                });
            }
            fat_clusters = needed;
        }
    }

    fn data_size(self) -> usize {
        self.clusters_per_card as usize * CLUSTER_SIZE
    }

    fn header(self) -> Result<[u8; HEADER_SIZE], ParseError> {
        let mut bytes = [0; HEADER_SIZE];
        bytes[..MAGIC.len()].copy_from_slice(MAGIC);
        bytes[0x1c..0x23].copy_from_slice(b"1.2.0.0");
        let mut cursor = Cursor::new(bytes.as_mut_slice());
        cursor.set_position(0x28);
        for value in [
            (PAGE_SIZE as u16).to_le_bytes(),
            (PAGES_PER_CLUSTER as u16).to_le_bytes(),
            (PAGES_PER_BLOCK as u16).to_le_bytes(),
            0xff00_u16.to_le_bytes(),
        ] {
            cursor.write_all(&value).expect("superblock fields fit");
        }
        for value in [
            self.clusters_per_card,
            self.alloc_offset,
            self.alloc_end,
            0,
            self.blocks - 1,
            self.blocks - 2,
        ] {
            cursor
                .write_all(&value.to_le_bytes())
                .expect("superblock fields fit");
        }
        cursor.set_position(0x50);
        cursor
            .write_all(&self.ifc_cluster.to_le_bytes())
            .expect("superblock fields fit");
        bytes[0xd0..0x150].fill(0xff);
        bytes[0x150] = 2;
        bytes[0x151] = 0x2b;
        Ok(bytes)
    }
}

struct Allocator {
    next: usize,
    limit: usize,
}

impl Allocator {
    fn take(&mut self, count: usize) -> Result<usize, ParseError> {
        let end = self.next.checked_add(count).ok_or(ParseError::Full {
            needed: usize::MAX,
            available: self.limit,
        })?;
        if end > self.limit {
            return Err(ParseError::Full {
                needed: end,
                available: self.limit,
            });
        }
        let start = self.next;
        self.next = end;
        Ok(start)
    }
}

impl MemoryCard<'static> {
    pub(in crate::impls::ps2) fn fits(
        capacity_blocks: u32,
        saves: &[Save<'_>],
    ) -> Result<bool, ParseError> {
        let geometry = Geometry::new(capacity_blocks)?;
        Ok(Self::required_clusters(saves)? <= geometry.alloc_end as usize)
    }

    pub(in crate::impls::ps2) fn format(
        capacity_blocks: u32,
        saves: &[Save<'_>],
        with_spare: bool,
    ) -> Result<Vec<u8>, ParseError> {
        let geometry = Geometry::new(capacity_blocks)?;
        if !Self::fits(capacity_blocks, saves)? {
            return Err(ParseError::Full {
                needed: Self::required_clusters(saves)?,
                available: geometry.alloc_end as usize,
            });
        }

        let mut image = vec![0xff; geometry.data_size()];
        image[..HEADER_SIZE].copy_from_slice(&geometry.header()?);
        let mut fat = vec![CHAIN_END; geometry.alloc_end as usize];
        let mut allocator = Allocator {
            next: 0,
            limit: geometry.alloc_end as usize,
        };

        let root_count = saves.len().checked_add(2).ok_or(ParseError::FileTooLarge)?;
        let root_start = allocator.take(Self::dirent_clusters(root_count)?)?;
        debug_assert_eq!(root_start, 0);
        let mut root = Vec::with_capacity(root_count * DIRENT_SIZE);
        root.extend_from_slice(&Self::dot_dirent(&[], b".", root_count, root_start)?);
        root.extend_from_slice(&Self::dot_dirent(&[], b"..", 0, root_start)?);

        for save in saves {
            let files = save.files().collect::<Vec<_>>();
            let directory_count = files.len().checked_add(2).ok_or(ParseError::FileTooLarge)?;
            let directory_start = allocator.take(Self::dirent_clusters(directory_count)?)?;
            let mut directory = Vec::with_capacity(directory_count * DIRENT_SIZE);
            directory.extend_from_slice(&Self::dot_dirent(
                save.raw_dirent(),
                b".",
                directory_count,
                directory_start,
            )?);
            directory.extend_from_slice(&Self::dot_dirent(
                save.raw_dirent(),
                b"..",
                0,
                root_start,
            )?);

            for file in files {
                let data = file.data();
                let cluster = if data.is_empty() {
                    u32::MAX as usize
                } else {
                    let start = allocator.take(data.len().div_ceil(CLUSTER_SIZE))?;
                    Self::write_run(&mut image, &mut fat, geometry, start, data);
                    start
                };
                directory.extend_from_slice(&Self::encoded_dirent(
                    file.raw_dirent(),
                    file.name_bytes(),
                    DEFAULT_FILE_MODE,
                    data.len(),
                    cluster,
                )?);
            }
            Self::write_run(&mut image, &mut fat, geometry, directory_start, &directory);
            root.extend_from_slice(&Self::encoded_dirent(
                save.raw_dirent(),
                save.name_bytes(),
                DEFAULT_DIRECTORY_MODE,
                directory_count,
                directory_start,
            )?);
        }
        Self::write_run(&mut image, &mut fat, geometry, root_start, &root);
        Self::write_fat(&mut image, geometry, &fat)?;

        if with_spare {
            Ok(Self::add_spare(&image))
        } else {
            Ok(image)
        }
    }

    pub(in crate::impls::ps2) fn formatted_superblock(
        capacity_blocks: u32,
    ) -> Result<Vec<u8>, ParseError> {
        let geometry = Geometry::new(capacity_blocks)?;
        let mut block = vec![0; ERASE_BLOCK_SIZE];
        block[..HEADER_SIZE].copy_from_slice(&geometry.header()?);
        Ok(block)
    }

    fn required_clusters(saves: &[Save<'_>]) -> Result<usize, ParseError> {
        let mut clusters =
            Self::dirent_clusters(saves.len().checked_add(2).ok_or(ParseError::FileTooLarge)?)?;
        for save in saves {
            let files = save.files();
            clusters = clusters
                .checked_add(Self::dirent_clusters(files.len() + 2)?)
                .ok_or(ParseError::FileTooLarge)?;
            for file in files {
                clusters = clusters
                    .checked_add(file.data().len().div_ceil(CLUSTER_SIZE))
                    .ok_or(ParseError::FileTooLarge)?;
            }
        }
        Ok(clusters)
    }

    fn dirent_clusters(count: usize) -> Result<usize, ParseError> {
        count
            .checked_mul(DIRENT_SIZE)
            .map(|bytes| bytes.div_ceil(CLUSTER_SIZE))
            .ok_or(ParseError::FileTooLarge)
    }

    fn write_run(
        image: &mut [u8],
        fat: &mut [u32],
        geometry: Geometry,
        start: usize,
        bytes: &[u8],
    ) {
        let clusters = bytes.len().div_ceil(CLUSTER_SIZE).max(1);
        for step in 0..clusters {
            let cluster = start + step;
            let offset = (geometry.alloc_offset as usize + cluster) * CLUSTER_SIZE;
            image[offset..offset + CLUSTER_SIZE].fill(0);
            let source = step * CLUSTER_SIZE;
            let length = (bytes.len() - source).min(CLUSTER_SIZE);
            image[offset..offset + length].copy_from_slice(&bytes[source..source + length]);
            fat[cluster] = ALLOCATED
                | if step + 1 == clusters {
                    CHAIN_END
                } else {
                    (cluster + 1) as u32
                };
        }
    }

    fn write_fat(image: &mut [u8], geometry: Geometry, fat: &[u32]) -> Result<(), ParseError> {
        let indirect_offset = geometry.ifc_cluster as usize * CLUSTER_SIZE;
        for index in 0..geometry.fat_clusters {
            let value = geometry.ifc_cluster + 1 + index;
            let offset = indirect_offset + index as usize * FAT_ENTRY_SIZE;
            image[offset..offset + FAT_ENTRY_SIZE].copy_from_slice(&value.to_le_bytes());
        }
        let entries_per_cluster = ENTRIES_PER_CLUSTER as usize;
        for (cluster, entry) in fat.iter().enumerate() {
            let fat_cluster = geometry.ifc_cluster as usize + 1 + cluster / entries_per_cluster;
            let offset =
                fat_cluster * CLUSTER_SIZE + (cluster % entries_per_cluster) * FAT_ENTRY_SIZE;
            image[offset..offset + FAT_ENTRY_SIZE].copy_from_slice(&entry.to_le_bytes());
        }
        Ok(())
    }

    fn encoded_dirent(
        raw: &[u8],
        name: &[u8],
        default_mode: u16,
        length: usize,
        cluster: usize,
    ) -> Result<[u8; DIRENT_SIZE], ParseError> {
        let mut bytes = [0; DIRENT_SIZE];
        let supplied_mode = raw
            .get(..2)
            .map(|mode| u16::from_le_bytes([mode[0], mode[1]]))
            .unwrap_or(0);
        if raw.len() >= DIRENT_SIZE && supplied_mode & USED_MODE != 0 {
            bytes.copy_from_slice(&raw[..DIRENT_SIZE]);
            let mode = (supplied_mode & !(FILE_MODE | DIRECTORY_MODE))
                | (default_mode & (FILE_MODE | DIRECTORY_MODE));
            bytes[..2].copy_from_slice(&mode.to_le_bytes());
        } else {
            bytes[..2].copy_from_slice(&default_mode.to_le_bytes());
            if name.len() > 32 {
                return Err(ParseError::NameTooLong(name.len()));
            }
            bytes[0x40..0x40 + name.len()].copy_from_slice(name);
        }
        let length = u32::try_from(length).map_err(|_| ParseError::ValueTooLarge(length))?;
        let cluster = u32::try_from(cluster).map_err(|_| ParseError::ValueTooLarge(cluster))?;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[16..20].copy_from_slice(&cluster.to_le_bytes());
        Ok(bytes)
    }

    fn dot_dirent(
        metadata: &[u8],
        name: &[u8],
        length: usize,
        cluster: usize,
    ) -> Result<[u8; DIRENT_SIZE], ParseError> {
        let mut bytes = [0; DIRENT_SIZE];
        bytes[..2].copy_from_slice(&DEFAULT_DIRECTORY_MODE.to_le_bytes());
        if metadata.len() >= DIRENT_SIZE {
            bytes[8..16].copy_from_slice(&metadata[8..16]);
            bytes[24..32].copy_from_slice(&metadata[24..32]);
        }
        let length = u32::try_from(length).map_err(|_| ParseError::ValueTooLarge(length))?;
        let cluster = u32::try_from(cluster).map_err(|_| ParseError::ValueTooLarge(cluster))?;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[16..20].copy_from_slice(&cluster.to_le_bytes());
        bytes[0x40..0x40 + name.len()].copy_from_slice(name);
        Ok(bytes)
    }

    fn add_spare(data: &[u8]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(data.len() / PAGE_SIZE * RAW_PAGE_SIZE);
        for page in data.chunks_exact(PAGE_SIZE) {
            raw.extend_from_slice(page);
            raw.extend_from_slice(&Self::spare(page));
        }
        raw
    }

    fn spare(page: &[u8]) -> [u8; SPARE_SIZE] {
        if page.iter().all(|byte| *byte == 0xff) {
            return [0xff; SPARE_SIZE];
        }
        let mut spare = [0; SPARE_SIZE];
        for (chunk, code) in page
            .chunks_exact(ECC_CHUNK_SIZE)
            .zip(spare.chunks_exact_mut(3))
        {
            code.copy_from_slice(&Self::ecc(chunk));
        }
        spare
    }

    fn ecc(data: &[u8]) -> [u8; 3] {
        let mut column = 0x77;
        let mut line_complement = 0x7f;
        let mut line = 0x7f;
        for (index, byte) in data.iter().copied().enumerate() {
            column ^= Self::column_parity(byte);
            if byte.count_ones() & 1 != 0 {
                let index = index as u8;
                line_complement ^= !index;
                line ^= index;
            }
        }
        [column, line_complement & 0x7f, line & 0x7f]
    }

    fn column_parity(byte: u8) -> u8 {
        [0x55, 0x33, 0x0f, 0x00, 0xaa, 0xcc, 0xf0]
            .into_iter()
            .enumerate()
            .fold(0, |parity, (index, mask)| {
                parity | (((byte & mask).count_ones() as u8 & 1) << index)
            })
    }
}

impl fmt::Debug for Superblock<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Superblock")
            .field("version", &self.version())
            .field("page_len", &self.page_len())
            .field("pages_per_cluster", &self.pages_per_cluster())
            .field("pages_per_block", &self.pages_per_block())
            .field("erase_block_size", &self.erase_block_size())
            .field("clusters_per_card", &self.clusters_per_card())
            .field("alloc_offset", &self.alloc_offset())
            .field("alloc_end", &self.alloc_end())
            .field("rootdir_cluster", &self.rootdir_cluster())
            .field("backup_blocks", &self.backup_blocks())
            .field("card_type", &self.card_type())
            .field("card_flags", &self.card_flags())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::impls::ps2) enum ParseError {
    TooShort(usize),
    InvalidLayout,
    InvalidMagic,
    InvalidVersion,
    InvalidGeometry(&'static str),
    InvalidRootDirectory(usize),
    InvalidBackupBlocks([usize; 2]),
    InvalidCardType(u8),
    TooManyIndirectFatClusters(usize),
    InvalidIndirectFatCluster {
        index: usize,
        cluster: usize,
    },
    InvalidCardSize {
        actual: usize,
        data: usize,
        raw: usize,
    },
    InvalidCardOffset(usize),
    NonContiguousField {
        offset: usize,
        len: usize,
    },
    ClusterOutOfRange(usize),
    FatEntryOutOfRange(usize),
    DirentOutOfRange(usize),
    MissingIndirectFatCluster(usize),
    InvalidFatCluster {
        index: usize,
        cluster: usize,
    },
    InvalidFatNext {
        cluster: usize,
        next: usize,
    },
    UnallocatedCluster(usize),
    DirectoryTooLarge(usize),
    InvalidDirectory(&'static str),
    ChainEndedEarly {
        start: usize,
        required: usize,
    },
    ChainLoop(usize),
    InvalidFile(&'static str),
    FileTooLarge,
    NameTooLong(usize),
    ValueTooLarge(usize),
    Full {
        needed: usize,
        available: usize,
    },
    DuplicateSave(String),
    ReadOnly,
    Psu(psu::ParseError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(size) => write!(
                formatter,
                "raw image is {size} bytes, shorter than its {HEADER_SIZE}-byte superblock header"
            ),
            Self::InvalidLayout => formatter.write_str("superblock has an invalid byte layout"),
            Self::InvalidMagic => formatter.write_str("superblock has invalid magic"),
            Self::InvalidVersion => formatter.write_str("superblock has an invalid version"),
            Self::InvalidGeometry(reason) => {
                write!(formatter, "superblock has invalid geometry: {reason}")
            }
            Self::InvalidRootDirectory(cluster) => write!(
                formatter,
                "superblock root directory starts at relative cluster {cluster}, not zero"
            ),
            Self::InvalidBackupBlocks(blocks) => {
                write!(formatter, "superblock has invalid backup blocks {blocks:?}")
            }
            Self::InvalidCardType(card_type) => {
                write!(
                    formatter,
                    "superblock has card type {card_type}, not PS2 type 2"
                )
            }
            Self::TooManyIndirectFatClusters(count) => write!(
                formatter,
                "superblock requires {count} indirect FAT clusters but has only {IFC_ENTRIES} slots"
            ),
            Self::InvalidIndirectFatCluster { index, cluster } => write!(
                formatter,
                "superblock indirect FAT slot {index} points outside system space at cluster {cluster}"
            ),
            Self::InvalidCardSize { actual, data, raw } => write!(
                formatter,
                "memory-card image is {actual} bytes instead of {data} data bytes or {raw} raw bytes"
            ),
            Self::InvalidCardOffset(offset) => {
                write!(
                    formatter,
                    "memory-card offset {offset} is outside the image"
                )
            }
            Self::NonContiguousField { offset, len } => write!(
                formatter,
                "the {len}-byte field at memory-card offset {offset} crosses a page boundary"
            ),
            Self::ClusterOutOfRange(cluster) => {
                write!(formatter, "cluster {cluster} is outside the card")
            }
            Self::FatEntryOutOfRange(index) => {
                write!(formatter, "FAT entry {index} is outside its cluster")
            }
            Self::DirentOutOfRange(index) => {
                write!(formatter, "directory entry {index} is outside its cluster")
            }
            Self::MissingIndirectFatCluster(index) => {
                write!(formatter, "indirect FAT slot {index} is missing")
            }
            Self::InvalidFatCluster { index, cluster } => write!(
                formatter,
                "indirect FAT entry {index} points outside system space at cluster {cluster}"
            ),
            Self::InvalidFatNext { cluster, next } => write!(
                formatter,
                "FAT entry for cluster {cluster} points outside allocation space at cluster {next}"
            ),
            Self::UnallocatedCluster(cluster) => {
                write!(
                    formatter,
                    "cluster {cluster} is in a chain but is not allocated"
                )
            }
            Self::DirectoryTooLarge(count) => {
                write!(formatter, "directory claims an invalid {count} entries")
            }
            Self::InvalidDirectory(reason) => write!(formatter, "invalid directory: {reason}"),
            Self::ChainEndedEarly { start, required } => write!(
                formatter,
                "cluster chain from {start} ends before its required {required} clusters"
            ),
            Self::ChainLoop(start) => {
                write!(formatter, "cluster chain from {start} contains a loop")
            }
            Self::InvalidFile(reason) => write!(formatter, "invalid file: {reason}"),
            Self::FileTooLarge => formatter.write_str("memory-card file is too large"),
            Self::NameTooLong(length) => {
                write!(formatter, "memory-card name is {length} bytes")
            }
            Self::ValueTooLarge(value) => {
                write!(formatter, "memory-card value {value} is too large")
            }
            Self::Full { needed, available } => write!(
                formatter,
                "memory card needs {needed} data clusters but has {available}"
            ),
            Self::DuplicateSave(name) => {
                write!(formatter, "memory card already contains save `{name}`")
            }
            Self::ReadOnly => formatter.write_str("memory-card view is read-only"),
            Self::Psu(error) => write!(formatter, "cannot encode PSU: {error}"),
        }
    }
}

impl From<psu::ParseError> for ParseError {
    fn from(error: psu::ParseError) -> Self {
        Self::Psu(error)
    }
}

impl From<ParseError> for crate::Error {
    fn from(error: ParseError) -> Self {
        Self::MalformedMetadata {
            file: "PlayStation 2 memory card".to_owned(),
            reason: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted() -> Vec<u8> {
        MemoryCard::format(1024, &[], false).unwrap()
    }

    fn with_save(spare: bool) -> Vec<u8> {
        let mut metadata = vec![0; DIRENT_SIZE];
        metadata[..2].copy_from_slice(&0x8497u16.to_le_bytes());
        metadata[8..16].copy_from_slice(b"created!");
        metadata[24..32].copy_from_slice(b"changed!");
        metadata[32..36].copy_from_slice(&0xfeed_beefu32.to_le_bytes());
        metadata[0x40..0x48].copy_from_slice(b"icon.sys");
        let save = Save::from_files(
            "BASLUS-00001",
            &[],
            [
                ("icon.sys", metadata.as_slice(), &b"icon"[..]),
                ("save.dat", &[][..], &b"save"[..]),
                ("settings", &[][..], &[][..]),
            ],
        )
        .unwrap();
        MemoryCard::format(1024, &[save], spare).unwrap()
    }

    #[test]
    fn parses_a_standard_card_without_copying_the_header() {
        let bytes = formatted();
        let superblock = Superblock::new(&bytes).unwrap();

        assert_eq!(superblock.version(), "1.2.0.0");
        assert_eq!(superblock.page_len(), 512);
        assert_eq!(superblock.pages_per_cluster(), 2);
        assert_eq!(superblock.pages_per_block(), 16);
        assert_eq!(superblock.cluster_size(), 1024);
        assert_eq!(superblock.erase_block_size(), 8192);
        assert_eq!(superblock.entries_per_cluster(), 256);
        assert_eq!(superblock.clusters_per_card(), 8192);
        assert_eq!(superblock.alloc_offset(), 41);
        assert_eq!(superblock.alloc_end(), 8135);
        assert_eq!(superblock.rootdir_cluster(), 0);
        assert_eq!(superblock.backup_blocks(), [1023, 1022]);
        assert_eq!(superblock.indirect_fat_cluster(0), Some(8));
        assert_eq!(superblock.card_type(), 2);

        let raw_address = superblock.raw as *const RawSuperblock as usize;
        assert_eq!(raw_address, bytes.as_ptr() as usize);
    }

    #[test]
    fn rejects_truncated_and_non_card_data() {
        assert_eq!(
            Superblock::new(&[0; HEADER_SIZE - 1]).unwrap_err(),
            ParseError::TooShort(HEADER_SIZE - 1)
        );
        assert_eq!(
            Superblock::new(&[0; HEADER_SIZE]).unwrap_err(),
            ParseError::InvalidMagic
        );
    }

    #[test]
    fn rejects_geometry_that_cannot_be_traversed_safely() {
        let mut bytes = formatted();
        bytes[0x38..0x3c].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            Superblock::new(&bytes).unwrap_err(),
            ParseError::InvalidGeometry("allocatable area is outside the card")
        );

        let mut bytes = formatted();
        bytes[0x50..0x54].copy_from_slice(&41u32.to_le_bytes());
        assert_eq!(
            Superblock::new(&bytes).unwrap_err(),
            ParseError::InvalidIndirectFatCluster {
                index: 0,
                cluster: 41,
            }
        );
    }

    #[test]
    fn parses_fat_chains_and_borrowed_dirents() {
        let bytes = with_save(false);
        let card = MemoryCard::new(&bytes, 1024).unwrap();

        assert!(!card.had_spare());
        assert_eq!(card.fat().entry(0).unwrap(), FatEntry::Next(1));
        assert_eq!(card.fat().entry(1).unwrap(), FatEntry::End);
        assert_eq!(card.fat().entry(100).unwrap(), FatEntry::Free);

        let root = card.root_directory().unwrap();
        assert_eq!(
            root.entries()
                .map(|entry| entry.name().into_owned())
                .collect::<Vec<_>>(),
            [".", "..", "BASLUS-00001"]
        );
        let save = root.entries().nth(2).unwrap();
        let directory = card.directory(save).unwrap();
        assert_eq!(
            directory
                .entries()
                .map(|entry| entry.name().into_owned())
                .collect::<Vec<_>>(),
            [".", "..", "icon.sys", "save.dat", "settings"]
        );

        let icon = directory.entries().nth(2).unwrap();
        assert!(icon.is_used());
        assert!(icon.is_file());
        assert_eq!(icon.created(), b"created!");
        assert_eq!(icon.modified(), b"changed!");
        assert_eq!(icon.attributes(), 0xfeed_beef);
        assert!(icon.as_bytes().as_ptr() >= bytes.as_ptr());
        assert!(icon.as_bytes().as_ptr() < bytes[bytes.len()..].as_ptr());
    }

    #[test]
    fn dirents_remain_zero_copy_when_pages_have_spare_data() {
        let bytes = with_save(true);
        let card = MemoryCard::new(&bytes, 1024).unwrap();

        assert!(card.had_spare());
        let root = card.root_directory().unwrap();
        let save = root.entries().nth(2).unwrap();
        let directory = card.directory(save).unwrap();
        assert_eq!(directory.entries().nth(2).unwrap().name(), "icon.sys");
        for entry in root.entries().chain(directory.entries()) {
            assert!(entry.as_bytes().as_ptr() >= bytes.as_ptr());
            assert!(entry.as_bytes().as_ptr() < bytes[bytes.len()..].as_ptr());
        }
    }

    #[test]
    fn rejects_directory_chains_that_end_early_or_loop() {
        let mut bytes = with_save(false);
        let fat_offset = {
            let card = MemoryCard::new(&bytes, 1024).unwrap();
            let indirect = card.superblock().indirect_fat_cluster(0).unwrap();
            let fat_cluster = card.raw_cluster_u32(indirect, 0).unwrap() as usize;
            fat_cluster * card.superblock().cluster_size()
        };

        bytes[fat_offset..fat_offset + 4].copy_from_slice(&(ALLOCATED | CHAIN_END).to_le_bytes());
        assert_eq!(
            MemoryCard::new(&bytes, 1024)
                .unwrap()
                .root_directory()
                .unwrap_err(),
            ParseError::ChainEndedEarly {
                start: 0,
                required: 2,
            }
        );

        bytes[fat_offset..fat_offset + 4].copy_from_slice(&ALLOCATED.to_le_bytes());
        assert_eq!(
            MemoryCard::new(&bytes, 1024)
                .unwrap()
                .root_directory()
                .unwrap_err(),
            ParseError::ChainLoop(0)
        );
    }

    #[test]
    fn decodes_dirent_names_as_shift_jis_without_changing_the_bytes() {
        let mut bytes = [0; DIRENT_SIZE];
        let (encoded, _, had_errors) = SHIFT_JIS.encode("設定");
        assert!(!had_errors);
        bytes[0x40..0x40 + encoded.len()].copy_from_slice(&encoded);
        let dirent = Dirent::ref_from_bytes(&bytes).unwrap();

        assert_eq!(dirent.name(), "設定");
        assert_eq!(dirent.name_bytes(), encoded.as_ref());
    }

    #[test]
    fn raw_image_trait_appends_to_mutable_backing() {
        let mut image = MemoryCard::format(1024, &[], true).unwrap();
        let save = Save::from_files(
            "BASLUS-00001",
            &[],
            [("save.dat", &[][..], &b"payload"[..])],
        )
        .unwrap();

        {
            let mut card: MemoryCard<'_> = RawMemoryCardImage::new_mut(&mut image, 1024).unwrap();
            RawMemoryCardImage::append_save(&mut card, save).unwrap();
        }

        let card: MemoryCard<'_> = RawMemoryCardImage::new(&image, 1024).unwrap();
        let saves = RawMemoryCardImage::saves(&card).unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].name(), "BASLUS-00001");
        assert_eq!(saves[0].files().next().unwrap().data(), b"payload");
    }

    #[test]
    fn append_uses_free_fat_clusters_without_reformatting() {
        let mut image = MemoryCard::format(1024, &[], false).unwrap();
        let parsed = MemoryCard::new(&image, 1024).unwrap();
        let alloc_offset = parsed.superblock().alloc_offset();
        let reserved_offset = parsed.fat_entry_offset(1).unwrap();
        image[reserved_offset..reserved_offset + FAT_ENTRY_SIZE]
            .copy_from_slice(&(ALLOCATED | CHAIN_END).to_le_bytes());
        let reserved_cluster =
            image[(alloc_offset + 1) * CLUSTER_SIZE..(alloc_offset + 2) * CLUSTER_SIZE].to_vec();
        let save = Save::from_files(
            "BASLUS-00001",
            &[],
            [("save.dat", &[][..], &b"payload"[..])],
        )
        .unwrap();

        let mut card = MemoryCard::new_mut(&mut image, 1024).unwrap();
        card.append_save(save).unwrap();
        assert_eq!(card.fat().entry(0).unwrap(), FatEntry::Next(2));
        assert_eq!(card.fat().entry(1).unwrap(), FatEntry::End);
        drop(card);

        assert_eq!(
            image[(alloc_offset + 1) * CLUSTER_SIZE..(alloc_offset + 2) * CLUSTER_SIZE],
            reserved_cluster
        );
        assert_eq!(
            MemoryCard::new(&image, 1024)
                .unwrap()
                .saves()
                .unwrap()
                .len(),
            1
        );
    }
}
