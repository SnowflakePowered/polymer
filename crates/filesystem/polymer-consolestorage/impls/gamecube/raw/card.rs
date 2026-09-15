use std::collections::HashSet;

use uuid::Uuid;

use super::super::gci::{self, Region, Save};
use crate::impls::memorycard::{CardBytes, RawMemoryCardImage};

pub const BLOCK_SIZE: usize = gci::BLOCK_SIZE;
pub const MC251_BLOCKS: u16 = 251;
const MAX_BLOCKS: u16 = 4091;
pub(in crate::impls::gamecube) const DIRECTORY_ENTRIES: usize = 127;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("the memory-card image has an invalid geometry")]
    InvalidGeometry,
    #[error("the memory-card header is invalid")]
    InvalidHeader,
    #[error("both copies of the memory-card {0} are invalid")]
    InvalidCopies(&'static str),
    #[error("the memory-card allocation chain is invalid")]
    InvalidAllocation,
    #[error("the memory card contains duplicate save identities")]
    DuplicateIdentity,
    #[error(transparent)]
    Gci(#[from] gci::ParseError),
    #[error("the selected saves require {required} blocks but the card has {available}")]
    CardFull { required: usize, available: u16 },
    #[error("the selected saves require more than 127 directory entries")]
    DirectoryFull,
    #[error("memory-card view is read-only")]
    ReadOnly,
    #[error("the save region has no known GameCube memory-card encoding")]
    UnknownEncoding,
    #[error("the save encoding is incompatible with the memory-card header")]
    EncodingMismatch,
}

pub struct MemoryCard<'a> {
    bytes: CardBytes<'a>,
    capacity_blocks: u16,
    encoding: Option<bool>,
}

impl<'a> MemoryCard<'a> {
    fn validate(bytes: &[u8], blocks: u32) -> Result<(u16, bool), ParseError> {
        let capacity_blocks = u16::try_from(blocks).map_err(|_| ParseError::InvalidGeometry)?;
        let physical_blocks = usize::from(capacity_blocks) + 5;
        if capacity_blocks > MAX_BLOCKS
            || physical_blocks % 16 != 0
            || bytes.len() != physical_blocks * BLOCK_SIZE
        {
            return Err(ParseError::InvalidGeometry);
        }
        let size_mbits = (physical_blocks / 16) as u16;
        let encoding = match u16::from_be_bytes([bytes[0x24], bytes[0x25]]) {
            0 => false,
            1 => true,
            _ => return Err(ParseError::InvalidHeader),
        };
        if u16::from_be_bytes([bytes[0x22], bytes[0x23]]) != size_mbits
            || !Self::checksum_matches(&bytes[..0x1fc], &bytes[0x1fc..0x200])
        {
            return Err(ParseError::InvalidHeader);
        }
        Ok((capacity_blocks, encoding))
    }

    fn region_encoding(region: Region) -> Result<bool, ParseError> {
        match region {
            Region::Japan => Ok(true),
            Region::Usa | Region::Pal => Ok(false),
            Region::Unknown(_) => Err(ParseError::UnknownEncoding),
        }
    }

    pub(in crate::impls::gamecube) fn initialize_header(
        &mut self,
        id: Uuid,
        region: Region,
    ) -> Result<(), ParseError> {
        let bytes = self.bytes.as_mut_slice().ok_or(ParseError::ReadOnly)?;
        let id = id.as_bytes();
        bytes[..12].copy_from_slice(&id[..12]);
        bytes[0x0c..0x14]
            .copy_from_slice(&u64::from_be_bytes(id[8..16].try_into().unwrap()).to_be_bytes());
        bytes[0x14..0x20].fill(0);
        self.initialize_encoding(Self::region_encoding(region)?);
        Ok(())
    }

    fn has_saves(&self) -> Result<bool, ParseError> {
        Ok(
            self.active_copy(1, 2)?[..DIRECTORY_ENTRIES * gci::DIRENT_SIZE]
                .chunks_exact(gci::DIRENT_SIZE)
                .any(|entry| entry[..4] != [0xff; 4]),
        )
    }

    fn initialize_encoding(&mut self, encoding: bool) {
        let bytes = self.bytes.as_mut_slice().expect("checked mutable");
        bytes[0x24..0x26].copy_from_slice(&u16::from(encoding).to_be_bytes());
        let (checksum, inverse) = Self::checksum(&bytes[..0x1fc]);
        bytes[0x1fc..0x1fe].copy_from_slice(&checksum.to_be_bytes());
        bytes[0x1fe..0x200].copy_from_slice(&inverse.to_be_bytes());
        self.encoding = Some(encoding);
    }

    fn active_copy(&self, first: usize, second: usize) -> Result<&[u8], ParseError> {
        let bytes = self.bytes.as_slice();
        let block = |index| &bytes[index * BLOCK_SIZE..(index + 1) * BLOCK_SIZE];
        let first_block = block(first);
        let second_block = block(second);
        let valid = |bytes: &[u8]| {
            if first == 1 {
                Self::checksum_matches(&bytes[..0x1ffc], &bytes[0x1ffc..])
            } else {
                Self::checksum_matches(&bytes[4..], &bytes[..4])
            }
        };
        match (valid(first_block), valid(second_block)) {
            (false, false) => Err(ParseError::InvalidCopies(if first == 1 {
                "directory"
            } else {
                "allocation table"
            })),
            (true, false) => Ok(first_block),
            (false, true) => Ok(second_block),
            (true, true) => {
                let counter = |bytes: &[u8]| {
                    let offset = if first == 1 { 0x1ffa } else { 4 };
                    i16::from_be_bytes([bytes[offset], bytes[offset + 1]])
                };
                Ok(if counter(first_block) >= counter(second_block) {
                    first_block
                } else {
                    second_block
                })
            }
        }
    }

    fn checksum_matches(data: &[u8], stored: &[u8]) -> bool {
        let (checksum, inverse) = Self::checksum(data);
        stored == [checksum.to_be_bytes(), inverse.to_be_bytes()].concat()
    }

    fn checksum(data: &[u8]) -> (u16, u16) {
        debug_assert_eq!(data.len() % 2, 0);
        let mut checksum = 0u16;
        let mut inverse = 0u16;
        for word in data.chunks_exact(2) {
            let word = u16::from_be_bytes([word[0], word[1]]);
            checksum = checksum.wrapping_add(word);
            inverse = inverse.wrapping_add(!word);
        }
        (
            if checksum == 0xffff { 0 } else { checksum },
            if inverse == 0xffff { 0 } else { inverse },
        )
    }
}

impl<'a> RawMemoryCardImage<'a> for MemoryCard<'a> {
    type Error = ParseError;
    type Save<'card> = Save<'card>;

    const BLOCK_SIZE: u64 = BLOCK_SIZE as u64;
    const MAX_BLOCKS: u32 = MAX_BLOCKS as u32;

    fn empty(blocks: u32) -> Result<Self, Self::Error> {
        let capacity_blocks = u16::try_from(blocks).map_err(|_| ParseError::InvalidGeometry)?;
        let physical_blocks = usize::from(capacity_blocks) + 5;
        if capacity_blocks > MAX_BLOCKS || physical_blocks % 16 != 0 {
            return Err(ParseError::InvalidGeometry);
        }

        let mut bytes = vec![0xff; physical_blocks * BLOCK_SIZE];
        bytes[..0x20].fill(0);
        bytes[0x20..0x22].copy_from_slice(&0u16.to_be_bytes());
        bytes[0x22..0x24]
            .copy_from_slice(&u16::try_from(physical_blocks / 16).unwrap().to_be_bytes());
        bytes[0x24..0x26].copy_from_slice(&0u16.to_be_bytes());
        let (checksum, inverse) = Self::checksum(&bytes[..0x1fc]);
        bytes[0x1fc..0x1fe].copy_from_slice(&checksum.to_be_bytes());
        bytes[0x1fe..0x200].copy_from_slice(&inverse.to_be_bytes());

        let mut directory = vec![0xff; BLOCK_SIZE];
        directory[0x1ffa..0x1ffc].copy_from_slice(&0i16.to_be_bytes());
        let mut bat = vec![0; BLOCK_SIZE];
        bat[4..6].copy_from_slice(&0i16.to_be_bytes());
        bat[6..8].copy_from_slice(&capacity_blocks.to_be_bytes());
        bat[8..10].copy_from_slice(&4u16.to_be_bytes());

        let (checksum, inverse) = Self::checksum(&directory[..0x1ffc]);
        directory[0x1ffc..0x1ffe].copy_from_slice(&checksum.to_be_bytes());
        directory[0x1ffe..].copy_from_slice(&inverse.to_be_bytes());
        let (checksum, inverse) = Self::checksum(&bat[4..]);
        bat[..2].copy_from_slice(&checksum.to_be_bytes());
        bat[2..4].copy_from_slice(&inverse.to_be_bytes());
        bytes[BLOCK_SIZE..BLOCK_SIZE * 2].copy_from_slice(&directory);
        bytes[BLOCK_SIZE * 2..BLOCK_SIZE * 3].copy_from_slice(&directory);
        bytes[BLOCK_SIZE * 3..BLOCK_SIZE * 4].copy_from_slice(&bat);
        bytes[BLOCK_SIZE * 4..BLOCK_SIZE * 5].copy_from_slice(&bat);
        Ok(Self {
            bytes: CardBytes::Owned(bytes),
            capacity_blocks,
            encoding: None,
        })
    }

    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error> {
        let (capacity_blocks, header_encoding) = Self::validate(bytes, blocks)?;
        let mut card = Self {
            bytes: CardBytes::Borrowed(bytes),
            capacity_blocks,
            encoding: Some(header_encoding),
        };
        if !card.has_saves()? {
            card.encoding = None;
        }
        Ok(card)
    }

    fn new_mut(bytes: &'a mut [u8], blocks: u32) -> Result<Self, Self::Error> {
        let (capacity_blocks, header_encoding) = Self::validate(bytes, blocks)?;
        let mut card = Self {
            bytes: CardBytes::BorrowedMut(bytes),
            capacity_blocks,
            encoding: Some(header_encoding),
        };
        if !card.has_saves()? {
            card.encoding = None;
        }
        Ok(card)
    }

    fn into_owned(self) -> Vec<u8> {
        self.bytes.into_owned()
    }

    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error> {
        let directory = self.active_copy(1, 2)?;
        let bat = self.active_copy(3, 4)?;
        let allocated_blocks = (0..self.capacity_blocks)
            .filter(|index| {
                let offset = 0x0a + usize::from(*index) * 2;
                bat[offset..offset + 2] != [0, 0]
            })
            .count();
        let recorded_free = u16::from_be_bytes([bat[6], bat[7]]);
        if usize::from(recorded_free) + allocated_blocks != usize::from(self.capacity_blocks) {
            return Err(ParseError::InvalidAllocation);
        }
        let mut used_blocks = HashSet::new();
        let mut identities = HashSet::new();
        let mut saves = Vec::new();
        for entry in
            directory[..DIRECTORY_ENTRIES * gci::DIRENT_SIZE].chunks_exact(gci::DIRENT_SIZE)
        {
            if entry[..4] == [0xff; 4] {
                continue;
            }
            let count = u16::from_be_bytes([entry[0x38], entry[0x39]]);
            let mut block = u16::from_be_bytes([entry[0x36], entry[0x37]]);
            if count == 0 || count > self.capacity_blocks {
                return Err(ParseError::InvalidAllocation);
            }
            let mut bytes = Vec::with_capacity(gci::DIRENT_SIZE + usize::from(count) * BLOCK_SIZE);
            bytes.extend_from_slice(entry);
            for index in 0..count {
                if !(5..self.capacity_blocks + 5).contains(&block) || !used_blocks.insert(block) {
                    return Err(ParseError::InvalidAllocation);
                }
                let offset = usize::from(block) * BLOCK_SIZE;
                bytes.extend_from_slice(&self.bytes.as_slice()[offset..offset + BLOCK_SIZE]);
                let map = 0x0a + usize::from(block - 5) * 2;
                let next = u16::from_be_bytes([bat[map], bat[map + 1]]);
                if index + 1 == count {
                    if next != 0xffff {
                        return Err(ParseError::InvalidAllocation);
                    }
                } else if next == 0 || next == 0xffff {
                    return Err(ParseError::InvalidAllocation);
                }
                block = next;
            }
            let save = Save::new(bytes)?.into_owned();
            if self.encoding != Some(Self::region_encoding(save.region())?) {
                return Err(ParseError::EncodingMismatch);
            }
            if !identities.insert(save.identity()) {
                return Err(ParseError::DuplicateIdentity);
            }
            saves.push(save);
        }
        if used_blocks.len() != allocated_blocks {
            return Err(ParseError::InvalidAllocation);
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
            return Err(ParseError::DuplicateIdentity);
        }
        let save_encoding = Self::region_encoding(save.region())?;
        if self
            .encoding
            .is_some_and(|encoding| encoding != save_encoding)
        {
            return Err(ParseError::EncodingMismatch);
        }

        let mut directory = self.active_copy(1, 2)?.to_vec();
        let mut bat = self.active_copy(3, 4)?.to_vec();
        let directory_index = directory[..DIRECTORY_ENTRIES * gci::DIRENT_SIZE]
            .chunks_exact(gci::DIRENT_SIZE)
            .position(|entry| entry[..4] == [0xff; 4])
            .ok_or(ParseError::DirectoryFull)?;
        let required = save.block_count();
        let available = u16::from_be_bytes([bat[6], bat[7]]);
        if required > available {
            return Err(ParseError::CardFull {
                required: usize::from(required),
                available,
            });
        }

        let last_allocated = u16::from_be_bytes([bat[8], bat[9]]);
        let first_candidate = if (5..self.capacity_blocks + 5).contains(&last_allocated) {
            last_allocated - 4
        } else {
            0
        };
        let mut allocation = Vec::with_capacity(usize::from(required));
        for step in 0..self.capacity_blocks {
            let index = (first_candidate + step) % self.capacity_blocks;
            let offset = 0x0a + usize::from(index) * 2;
            if bat[offset..offset + 2] == [0, 0] {
                allocation.push(index + 5);
                if allocation.len() == usize::from(required) {
                    break;
                }
            }
        }
        if allocation.len() != usize::from(required) {
            return Err(ParseError::InvalidAllocation);
        }

        if self.encoding.is_none() {
            self.initialize_encoding(save_encoding);
        }

        let entry = &mut directory
            [directory_index * gci::DIRENT_SIZE..(directory_index + 1) * gci::DIRENT_SIZE];
        entry.copy_from_slice(save.dirent());
        entry[0x36..0x38].copy_from_slice(&allocation[0].to_be_bytes());
        for (index, &block) in allocation.iter().enumerate() {
            let source = index * BLOCK_SIZE;
            let destination = usize::from(block) * BLOCK_SIZE;
            self.bytes.as_mut_slice().expect("checked mutable")
                [destination..destination + BLOCK_SIZE]
                .copy_from_slice(&save.data()[source..source + BLOCK_SIZE]);
            let next = allocation.get(index + 1).copied().unwrap_or(0xffff);
            let map = 0x0a + usize::from(block - 5) * 2;
            bat[map..map + 2].copy_from_slice(&next.to_be_bytes());
        }

        let directory_counter =
            i16::from_be_bytes([directory[0x1ffa], directory[0x1ffb]]).wrapping_add(1);
        directory[0x1ffa..0x1ffc].copy_from_slice(&directory_counter.to_be_bytes());
        let (checksum, inverse) = Self::checksum(&directory[..0x1ffc]);
        directory[0x1ffc..0x1ffe].copy_from_slice(&checksum.to_be_bytes());
        directory[0x1ffe..].copy_from_slice(&inverse.to_be_bytes());

        let bat_counter = i16::from_be_bytes([bat[4], bat[5]]).wrapping_add(1);
        bat[4..6].copy_from_slice(&bat_counter.to_be_bytes());
        bat[6..8].copy_from_slice(&(available - required).to_be_bytes());
        bat[8..10].copy_from_slice(&allocation[allocation.len() - 1].to_be_bytes());
        let (checksum, inverse) = Self::checksum(&bat[4..]);
        bat[..2].copy_from_slice(&checksum.to_be_bytes());
        bat[2..4].copy_from_slice(&inverse.to_be_bytes());

        let bytes = self.bytes.as_mut_slice().expect("checked mutable");
        bytes[BLOCK_SIZE..BLOCK_SIZE * 2].copy_from_slice(&directory);
        bytes[BLOCK_SIZE * 2..BLOCK_SIZE * 3].copy_from_slice(&directory);
        bytes[BLOCK_SIZE * 3..BLOCK_SIZE * 4].copy_from_slice(&bat);
        bytes[BLOCK_SIZE * 4..BLOCK_SIZE * 5].copy_from_slice(&bat);
        Ok(())
    }
}
