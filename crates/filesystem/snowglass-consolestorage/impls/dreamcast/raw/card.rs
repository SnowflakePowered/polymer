use std::collections::HashSet;

use zerocopy::byteorder::little_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::super::vmi::Vmi;
use super::super::vms::{self, Save};
use crate::impls::memorycard::{CardBytes, RawMemoryCardImage};

pub const BLOCK_SIZE: usize = 512;
pub const USER_BLOCKS: u32 = 200;
const PHYSICAL_BLOCKS: usize = 256;
const RAW_SIZE: usize = PHYSICAL_BLOCKS * BLOCK_SIZE;
const ROOT_BLOCK: u16 = 255;
const FAT_BLOCK: u16 = 254;
const DIRECTORY_BLOCK: u16 = 253;
const DIRECTORY_BLOCKS: u16 = 13;
const DIRECTORY_ENTRIES: usize = 200;
const FAT_FREE: u16 = 0xfffc;
const FAT_END: u16 = 0xfffa;
const FILE_DATA: u8 = 0x33;
const FILE_GAME: u8 = 0xcc;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Dirent {
    file_type: u8,
    protected: u8,
    first_block: U16,
    file_name: [u8; 12],
    timestamp: [u8; 8],
    block_count: U16,
    header_offset: U16,
    reserved: [u8; 4],
}

const _: () = assert!(size_of::<Dirent>() == 32);

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("a VMU image has invalid geometry")]
    InvalidGeometry,
    #[error("the VMU root block is invalid")]
    InvalidRoot,
    #[error("the VMU allocation chain is invalid")]
    InvalidAllocation,
    #[error("the VMU directory is full")]
    DirectoryFull,
    #[error("the VMU does not have enough free blocks")]
    CardFull,
    #[error("the VMU contains duplicate native filenames")]
    DuplicateSave,
    #[error("the VMU image is read-only")]
    ReadOnly,
    #[error(transparent)]
    Vmi(#[from] super::super::vmi::ParseError),
    #[error(transparent)]
    Vms(#[from] vms::ParseError),
}

pub struct VmuMemory<'a> {
    bytes: CardBytes<'a>,
    capacity_blocks: u16,
}

impl VmuMemory<'_> {
    fn root(bytes: &[u8]) -> Result<&[u8], ParseError> {
        let root = &bytes[usize::from(ROOT_BLOCK) * BLOCK_SIZE..RAW_SIZE];
        root[..16]
            .iter()
            .all(|byte| *byte == 0x55)
            .then_some(root)
            .ok_or(ParseError::InvalidRoot)
    }

    fn root_field(root: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([root[offset], root[offset + 1]])
    }

    fn validate(bytes: &[u8], blocks: u32) -> Result<u16, ParseError> {
        let capacity_blocks = u16::try_from(blocks).map_err(|_| ParseError::InvalidGeometry)?;
        if bytes.len() != RAW_SIZE || capacity_blocks == 0 || blocks > USER_BLOCKS {
            return Err(ParseError::InvalidGeometry);
        }
        let root = Self::root(bytes)?;
        if Self::root_field(root, 0x46) != FAT_BLOCK
            || Self::root_field(root, 0x48) != 1
            || Self::root_field(root, 0x4a) != DIRECTORY_BLOCK
            || Self::root_field(root, 0x4c) != DIRECTORY_BLOCKS
            || Self::root_field(root, 0x50) != capacity_blocks
        {
            return Err(ParseError::InvalidRoot);
        }
        Ok(capacity_blocks)
    }

    fn fat(&self) -> &[u8] {
        &self.bytes.as_slice()
            [usize::from(FAT_BLOCK) * BLOCK_SIZE..usize::from(FAT_BLOCK + 1) * BLOCK_SIZE]
    }

    fn fat_entry(&self, block: u16) -> u16 {
        let offset = usize::from(block) * 2;
        u16::from_le_bytes([self.fat()[offset], self.fat()[offset + 1]])
    }

    fn chain(&self, first: u16, count: u16) -> Result<Vec<u16>, ParseError> {
        if count == 0 {
            return Err(ParseError::InvalidAllocation);
        }
        let mut blocks = Vec::with_capacity(usize::from(count));
        let mut seen = HashSet::new();
        let mut block = first;
        for index in 0..count {
            if block >= PHYSICAL_BLOCKS as u16 || !seen.insert(block) {
                return Err(ParseError::InvalidAllocation);
            }
            blocks.push(block);
            let next = self.fat_entry(block);
            if index + 1 == count {
                if next != FAT_END {
                    return Err(ParseError::InvalidAllocation);
                }
            } else if next > u16::try_from(PHYSICAL_BLOCKS - 1).unwrap() {
                return Err(ParseError::InvalidAllocation);
            } else {
                block = next;
            }
        }
        Ok(blocks)
    }

    fn directory_blocks(&self) -> Result<Vec<u16>, ParseError> {
        self.chain(DIRECTORY_BLOCK, DIRECTORY_BLOCKS)
    }

    fn directory_entries(&self) -> Result<Vec<&Dirent>, ParseError> {
        let mut entries = Vec::with_capacity(DIRECTORY_ENTRIES);
        for block in self.directory_blocks()? {
            let offset = usize::from(block) * BLOCK_SIZE;
            for bytes in self.bytes.as_slice()[offset..offset + BLOCK_SIZE].chunks_exact(32) {
                entries.push(Dirent::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidRoot)?);
                if entries.len() == DIRECTORY_ENTRIES {
                    return Ok(entries);
                }
            }
        }
        Ok(entries)
    }

    fn first_free_directory_entry(&self) -> Result<usize, ParseError> {
        let mut visited = 0;
        for block in self.directory_blocks()? {
            let offset = usize::from(block) * BLOCK_SIZE;
            for (index, bytes) in self.bytes.as_slice()[offset..offset + BLOCK_SIZE]
                .chunks_exact(32)
                .enumerate()
            {
                if visited == DIRECTORY_ENTRIES {
                    return Err(ParseError::DirectoryFull);
                }
                visited += 1;
                let entry = Dirent::ref_from_bytes(bytes).map_err(|_| ParseError::InvalidRoot)?;
                if entry.file_type == 0 {
                    return Ok(offset + index * 32);
                }
            }
        }
        Err(ParseError::DirectoryFull)
    }
}

impl<'a> RawMemoryCardImage<'a> for VmuMemory<'a> {
    type Error = ParseError;
    type Save<'card> = Save<'card>;

    const BLOCK_SIZE: u64 = BLOCK_SIZE as u64;
    const MAX_BLOCKS: u32 = USER_BLOCKS;

    fn empty(blocks: u32) -> Result<Self, Self::Error> {
        let capacity_blocks = u16::try_from(blocks).map_err(|_| ParseError::InvalidGeometry)?;
        if capacity_blocks == 0 || blocks > USER_BLOCKS {
            return Err(ParseError::InvalidGeometry);
        }

        let mut bytes = vec![0; RAW_SIZE];
        let root = &mut bytes[usize::from(ROOT_BLOCK) * BLOCK_SIZE..RAW_SIZE];
        root[..16].fill(0x55);
        root[0x40..0x42].copy_from_slice(&ROOT_BLOCK.to_le_bytes());
        root[0x44..0x46].copy_from_slice(&ROOT_BLOCK.to_le_bytes());
        root[0x46..0x48].copy_from_slice(&FAT_BLOCK.to_le_bytes());
        root[0x48..0x4a].copy_from_slice(&1u16.to_le_bytes());
        root[0x4a..0x4c].copy_from_slice(&DIRECTORY_BLOCK.to_le_bytes());
        root[0x4c..0x4e].copy_from_slice(&DIRECTORY_BLOCKS.to_le_bytes());
        root[0x50..0x52].copy_from_slice(&capacity_blocks.to_le_bytes());

        let fat = &mut bytes
            [usize::from(FAT_BLOCK) * BLOCK_SIZE..usize::from(FAT_BLOCK + 1) * BLOCK_SIZE];
        for entry in fat.chunks_exact_mut(2) {
            entry.copy_from_slice(&FAT_FREE.to_le_bytes());
        }
        let directory_last = DIRECTORY_BLOCK - DIRECTORY_BLOCKS + 1;
        for block in directory_last + 1..=DIRECTORY_BLOCK {
            let offset = usize::from(block) * 2;
            fat[offset..offset + 2].copy_from_slice(&(block - 1).to_le_bytes());
        }
        for block in [directory_last, FAT_BLOCK, ROOT_BLOCK] {
            let offset = usize::from(block) * 2;
            fat[offset..offset + 2].copy_from_slice(&FAT_END.to_le_bytes());
        }

        Ok(Self {
            bytes: CardBytes::Owned(bytes),
            capacity_blocks,
        })
    }

    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error> {
        Ok(Self {
            capacity_blocks: Self::validate(bytes, blocks)?,
            bytes: CardBytes::Borrowed(bytes),
        })
    }

    fn new_mut(bytes: &'a mut [u8], blocks: u32) -> Result<Self, Self::Error> {
        let capacity_blocks = Self::validate(bytes, blocks)?;
        Ok(Self {
            bytes: CardBytes::BorrowedMut(bytes),
            capacity_blocks,
        })
    }

    fn into_owned(self) -> Vec<u8> {
        self.bytes.into_owned()
    }

    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error> {
        let mut saves = Vec::new();
        let mut identities = HashSet::new();
        let mut claimed = HashSet::new();
        for entry in self.directory_entries()? {
            if !matches!(entry.file_type, FILE_DATA | FILE_GAME) {
                continue;
            }
            if !identities.insert(entry.file_name) {
                return Err(ParseError::DuplicateSave);
            }
            let blocks = self.chain(entry.first_block.get(), entry.block_count.get())?;
            let mut vms = Vec::with_capacity(blocks.len() * BLOCK_SIZE);
            for block in blocks {
                if block >= self.capacity_blocks || !claimed.insert(block) {
                    return Err(ParseError::InvalidAllocation);
                }
                let offset = usize::from(block) * BLOCK_SIZE;
                vms.extend_from_slice(&self.bytes.as_slice()[offset..offset + BLOCK_SIZE]);
            }

            let mut resource_name = [0; 8];
            resource_name.copy_from_slice(&entry.file_name[..8]);
            for byte in &mut resource_name {
                if !byte.is_ascii_alphanumeric() && !matches!(*byte, b'_' | b'-') {
                    *byte = b'_';
                }
            }
            if entry.header_offset.get() != u16::from(entry.file_type == FILE_GAME) {
                return Err(ParseError::InvalidAllocation);
            }
            let mode =
                u16::from(entry.protected != 0) | (u16::from(entry.file_type == FILE_GAME) << 1);
            let initial = Vmi::synthesize(
                resource_name,
                entry.file_name,
                &entry.file_name,
                b"Dreamcast VMU",
                entry.timestamp,
                mode,
                vms.len(),
            )?;
            let initial_save = Save::from_parts(initial, vms)?;
            let header = initial_save.header()?;
            let vmi = Vmi::synthesize(
                resource_name,
                entry.file_name,
                header.long_description_bytes(),
                header.application_id(),
                entry.timestamp,
                mode,
                initial_save.vms().len(),
            )?;
            saves.push(Save::from_parts(vmi, initial_save.vms().to_vec())?);
        }
        Ok(saves)
    }

    fn append_save(&mut self, save: Self::Save<'_>) -> Result<(), Self::Error> {
        if self.bytes.as_mut_slice().is_none() {
            return Err(ParseError::ReadOnly);
        }
        if self
            .saves()?
            .iter()
            .any(|existing| existing.identity() == save.identity())
        {
            return Err(ParseError::DuplicateSave);
        }
        let required = save.block_count();
        if required == 0 || required > usize::from(self.capacity_blocks) {
            return Err(ParseError::CardFull);
        }
        let free = (0..self.capacity_blocks)
            .filter(|block| self.fat_entry(*block) == FAT_FREE)
            .collect::<Vec<_>>();
        let allocation = if save.vmi().is_game() {
            (0..=self.capacity_blocks - u16::try_from(required).unwrap())
                .find_map(|first| {
                    (first..first + u16::try_from(required).unwrap())
                        .all(|block| self.fat_entry(block) == FAT_FREE)
                        .then(|| (first..first + u16::try_from(required).unwrap()).collect())
                })
                .ok_or(ParseError::CardFull)?
        } else {
            if free.len() < required {
                return Err(ParseError::CardFull);
            }
            free.into_iter().rev().take(required).collect::<Vec<_>>()
        };
        let directory_offset = self.first_free_directory_entry()?;

        let bytes = self.bytes.as_mut_slice().expect("checked mutable");
        for (index, block) in allocation.iter().copied().enumerate() {
            let source = index * BLOCK_SIZE;
            let destination = usize::from(block) * BLOCK_SIZE;
            bytes[destination..destination + BLOCK_SIZE].fill(0);
            let copied = (save.vms().len() - source).min(BLOCK_SIZE);
            bytes[destination..destination + copied]
                .copy_from_slice(&save.vms()[source..source + copied]);

            let next = allocation.get(index + 1).copied().unwrap_or(FAT_END);
            let fat = usize::from(FAT_BLOCK) * BLOCK_SIZE + usize::from(block) * 2;
            bytes[fat..fat + 2].copy_from_slice(&next.to_le_bytes());
        }

        let entry = Dirent::mut_from_bytes(&mut bytes[directory_offset..directory_offset + 32])
            .map_err(|_| ParseError::InvalidRoot)?;
        entry.file_type = if save.vmi().is_game() {
            FILE_GAME
        } else {
            FILE_DATA
        };
        entry.protected = if save.vmi().is_protected() { 0xff } else { 0 };
        entry.first_block = U16::new(allocation[0]);
        entry.file_name = save.identity();
        entry.timestamp = save.vmi().timestamp();
        entry.block_count = U16::new(u16::try_from(required).unwrap());
        entry.header_offset = U16::new(u16::from(save.vmi().is_game()));
        entry.reserved.fill(0);
        Ok(())
    }
}
