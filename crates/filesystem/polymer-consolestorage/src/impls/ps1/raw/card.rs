use std::borrow::Cow;
use std::fmt;

use encoding_rs::SHIFT_JIS;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{U16, U32},
};

use crate::impls::memorycard::{CardBytes, RawMemoryCardImage};

pub(crate) const BLOCK_SIZE: usize = 8 * 1024;
pub(crate) const FRAME_SIZE: usize = 128;
pub(crate) const DATA_BLOCKS: usize = 15;
pub(crate) const RAW_SIZE: usize = BLOCK_SIZE * (DATA_BLOCKS + 1);
pub(crate) const NATIVE_ID_SIZE: usize = 21;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub(crate) enum AllocationState {
    AllocFirst = 0x51,
    AllocMid = 0x52,
    AllocLast = 0x53,
    Free = 0xa0,
    FreeFirst = 0xa1,
    FreeMid = 0xa2,
    FreeLast = 0xa3,
}

impl AllocationState {
    pub(crate) const fn raw(self) -> u32 {
        self as u32
    }

    fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            0x51 => Self::AllocFirst,
            0x52 => Self::AllocMid,
            0x53 => Self::AllocLast,
            0xa0 => Self::Free,
            0xa1 => Self::FreeFirst,
            0xa2 => Self::FreeMid,
            0xa3 => Self::FreeLast,
            _ => return None,
        })
    }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct HeaderFrame {
    id: [u8; 2],
    pad: [u8; 125],
    checksum: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct DirectoryFrame {
    state: U32,
    filesize: U32,
    next_block: U16,
    filename: [u8; NATIVE_ID_SIZE],
    pad: [u8; 96],
    checksum: u8,
}

impl DirectoryFrame {
    pub(crate) fn state(&self) -> Option<AllocationState> {
        AllocationState::from_raw(self.state.get())
    }

    pub(crate) fn filesize(&self) -> u32 {
        self.filesize.get()
    }

    pub(crate) fn next_block(&self) -> u16 {
        self.next_block.get()
    }

    pub(crate) fn filename(&self) -> &[u8; NATIVE_ID_SIZE] {
        &self.filename
    }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct TitleFrame {
    id: [u8; 2],
    metadata: [u8; 2],
    title: [u8; 64],
    remaining_metadata: [u8; 60],
}

impl TitleFrame {
    fn decode_title(&self) -> Cow<'_, str> {
        let end = self
            .title
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(self.title.len());
        let (title, _) = SHIFT_JIS.decode_without_bom_handling(&self.title[..end]);
        title
    }
}

const _: () = assert!(size_of::<HeaderFrame>() == FRAME_SIZE);
const _: () = assert!(size_of::<DirectoryFrame>() == FRAME_SIZE);
const _: () = assert!(size_of::<TitleFrame>() == FRAME_SIZE);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Block<'a> {
    bytes: &'a [u8],
}

impl<'a> Block<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, ParseError> {
        if bytes.len() != BLOCK_SIZE {
            return Err(ParseError::InvalidBlockSize(bytes.len()));
        }
        Ok(Self { bytes })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DataBlock<'a> {
    title: &'a TitleFrame,
}

impl<'a> DataBlock<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Self, ParseError> {
        Block::new(bytes)?;
        let title = TitleFrame::ref_from_bytes(&bytes[..FRAME_SIZE])
            .map_err(|_| ParseError::InvalidLayout("title frame"))?;
        if title.id != *b"SC" {
            return Err(ParseError::InvalidDataBlockHeader(title.id));
        }
        Ok(Self { title })
    }

    pub(crate) fn title(&self) -> &'a TitleFrame {
        self.title
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InfoBlock<'a> {
    block: Block<'a>,
}

impl<'a> InfoBlock<'a> {
    fn validate_checksum(frame: &[u8], index: usize) -> Result<(), ParseError> {
        let expected = frame[..FRAME_SIZE - 1]
            .iter()
            .fold(0, |checksum, byte| checksum ^ byte);
        (expected == frame[FRAME_SIZE - 1])
            .then_some(())
            .ok_or(ParseError::InvalidChecksum(index))
    }

    fn new(block: Block<'a>) -> Result<Self, ParseError> {
        let header = HeaderFrame::ref_from_bytes(&block.bytes[..FRAME_SIZE])
            .map_err(|_| ParseError::InvalidLayout("header frame"))?;
        if header.id != *b"MC" {
            return Err(ParseError::InvalidCardHeader(header.id));
        }
        Self::validate_checksum(&block.bytes[..FRAME_SIZE], 0)?;
        for index in 0..DATA_BLOCKS {
            let frame = index + 1;
            Self::validate_checksum(
                &block.bytes[frame * FRAME_SIZE..(frame + 1) * FRAME_SIZE],
                frame,
            )?;
        }
        Ok(Self { block })
    }

    pub(crate) fn directory_frames(self) -> impl ExactSizeIterator<Item = &'a DirectoryFrame> {
        (0..DATA_BLOCKS).map(move |index| {
            self.directory_frame(index)
                .expect("the directory has exactly one entry per data block")
        })
    }

    pub(crate) fn directory_frame(self, index: usize) -> Option<&'a DirectoryFrame> {
        if index >= DATA_BLOCKS {
            return None;
        }
        let start = (index + 1) * FRAME_SIZE;
        DirectoryFrame::ref_from_bytes(&self.block.bytes[start..start + FRAME_SIZE]).ok()
    }
}

#[derive(Debug)]
pub(crate) struct MemoryCard<'a> {
    bytes: CardBytes<'a>,
    next_block: usize,
    capacity_blocks: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct Save<'a> {
    native_id: &'a [u8; NATIVE_ID_SIZE],
    blocks: [Option<&'a [u8]>; DATA_BLOCKS],
    block_count: u8,
    file_size: usize,
}

pub(crate) struct SaveHeader<'a> {
    pub(crate) title: Cow<'a, str>,
}

impl<'a> Save<'a> {
    pub(crate) fn new(
        native_id: &'a [u8; NATIVE_ID_SIZE],
        payload: &'a [u8],
    ) -> Result<Self, ParseError> {
        let block_count = payload.len().max(1).div_ceil(BLOCK_SIZE);
        if block_count > DATA_BLOCKS {
            return Err(ParseError::Malformed(
                "save payload exceeds the memory card capacity",
            ));
        }
        let mut blocks = [None; DATA_BLOCKS];
        for (slot, block) in blocks.iter_mut().zip(payload.chunks(BLOCK_SIZE)) {
            *slot = Some(block);
        }
        if payload.is_empty() {
            blocks[0] = Some(payload);
        }
        Ok(Self {
            native_id,
            blocks,
            block_count: block_count as u8,
            file_size: payload.len(),
        })
    }

    pub(crate) fn native_id(&self) -> &'a [u8; NATIVE_ID_SIZE] {
        self.native_id
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.blocks().map(<[u8]>::len).sum()
    }

    pub(crate) fn block_count(&self) -> usize {
        usize::from(self.block_count)
    }

    pub(crate) fn blocks(&self) -> impl ExactSizeIterator<Item = &'a [u8]> + '_ {
        self.blocks[..usize::from(self.block_count)]
            .iter()
            .map(|block| block.expect("every save block is initialized"))
    }

    pub(crate) fn describe(&self) -> Result<SaveHeader<'a>, ParseError> {
        let block = DataBlock::new(
            self.blocks()
                .next()
                .expect("a save always occupies at least one block"),
        )?;
        Ok(SaveHeader {
            title: block.title().decode_title(),
        })
    }
}

impl<'a> MemoryCard<'a> {
    fn validate_capacity(blocks: u32) -> Result<usize, ParseError> {
        let blocks = usize::try_from(blocks).map_err(|_| ParseError::InvalidCapacity(blocks))?;
        if blocks == 0 || blocks > DATA_BLOCKS {
            return Err(ParseError::InvalidCapacity(blocks as u32));
        }
        Ok(blocks)
    }

    fn find_next_block(bytes: &[u8], capacity_blocks: usize) -> Result<usize, ParseError> {
        if bytes.len() != RAW_SIZE {
            return Err(ParseError::InvalidCardSize(bytes.len()));
        }
        let info = InfoBlock::new(Block::new(&bytes[..BLOCK_SIZE])?)?;
        let next_block = info
            .directory_frames()
            .enumerate()
            .filter(|(_, directory)| {
                matches!(
                    directory.state(),
                    Some(
                        AllocationState::AllocFirst
                            | AllocationState::AllocMid
                            | AllocationState::AllocLast
                    )
                )
            })
            .map(|(block, _)| block + 1)
            .max()
            .unwrap_or(0);
        if next_block > capacity_blocks {
            return Err(ParseError::Full);
        }
        Ok(next_block)
    }

    pub(crate) fn next_block(&self) -> usize {
        self.next_block
    }

    fn allocation_chain(
        info: InfoBlock<'_>,
        first: usize,
    ) -> Result<([u8; DATA_BLOCKS], usize), ParseError> {
        let mut chain = [0; DATA_BLOCKS];
        let mut seen = [false; DATA_BLOCKS];
        let mut count = 0;
        let mut block = first;

        loop {
            if block >= DATA_BLOCKS || seen[block] {
                return Err(ParseError::Malformed(
                    "save contains an invalid allocation chain",
                ));
            }
            seen[block] = true;
            chain[count] = block as u8;
            count += 1;

            let entry = info.directory_frame(block).ok_or(ParseError::Malformed(
                "save contains an invalid allocation chain",
            ))?;
            let next = entry.next_block();
            let expected = if block == first {
                AllocationState::AllocFirst
            } else if next == u16::MAX {
                AllocationState::AllocLast
            } else {
                AllocationState::AllocMid
            };
            if entry.state() != Some(expected) {
                return Err(ParseError::Malformed(
                    "save allocation chain has incompatible states",
                ));
            }
            if next == u16::MAX {
                return Ok((chain, count));
            }
            block = usize::from(next);
        }
    }

    pub(crate) fn update_checksum(frame: &mut [u8]) {
        frame[FRAME_SIZE - 1] = frame[..FRAME_SIZE - 1]
            .iter()
            .fold(0, |checksum, byte| checksum ^ byte);
    }
}

impl<'a> RawMemoryCardImage<'a> for MemoryCard<'a> {
    type Error = ParseError;
    type Save<'card> = Save<'card>;

    const BLOCK_SIZE: u64 = BLOCK_SIZE as u64;
    const MAX_BLOCKS: u32 = DATA_BLOCKS as u32;

    fn empty(blocks: u32) -> Result<Self, Self::Error> {
        let capacity_blocks = Self::validate_capacity(blocks)?;
        let mut bytes = vec![0xff; RAW_SIZE];
        bytes[..BLOCK_SIZE].fill(0);
        bytes[..2].copy_from_slice(b"MC");
        for frame in 0..64 {
            Self::update_checksum(&mut bytes[frame * FRAME_SIZE..(frame + 1) * FRAME_SIZE]);
        }
        for block in 0..DATA_BLOCKS {
            let frame = &mut bytes[(block + 1) * FRAME_SIZE..(block + 2) * FRAME_SIZE];
            frame.fill(0);
            frame[..4].copy_from_slice(&AllocationState::Free.raw().to_le_bytes());
            frame[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
            Self::update_checksum(frame);
        }
        Ok(Self {
            bytes: CardBytes::Owned(bytes),
            next_block: 0,
            capacity_blocks,
        })
    }

    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error> {
        let capacity_blocks = Self::validate_capacity(blocks)?;
        if bytes.len() != RAW_SIZE {
            return Err(ParseError::InvalidCardSize(bytes.len()));
        }
        let next_block = Self::find_next_block(bytes, capacity_blocks)?;
        Ok(Self {
            bytes: CardBytes::Borrowed(bytes),
            next_block,
            capacity_blocks,
        })
    }

    fn into_owned(self) -> Vec<u8> {
        self.bytes.into_owned()
    }

    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error> {
        let bytes = self.bytes.as_slice();
        let info = InfoBlock::new(Block::new(&bytes[..BLOCK_SIZE])?)?;
        let mut saves = Vec::<Save<'_>>::new();
        let mut claimed = [false; DATA_BLOCKS];

        for (first, directory) in info.directory_frames().enumerate() {
            if directory.state() != Some(AllocationState::AllocFirst) {
                continue;
            }

            let (blocks, block_count) = Self::allocation_chain(info, first)?;
            for &block in &blocks[..block_count] {
                if std::mem::replace(&mut claimed[usize::from(block)], true) {
                    return Err(ParseError::Malformed("save allocation chains overlap"));
                }
            }
            if saves
                .iter()
                .any(|save| save.native_id == directory.filename())
            {
                return Err(ParseError::Malformed(
                    "multiple saves have the same native identity",
                ));
            }
            if directory.filesize() as usize > block_count * BLOCK_SIZE {
                return Err(ParseError::Malformed(
                    "save file size exceeds its allocation chain",
                ));
            }

            let mut payload = [None; DATA_BLOCKS];
            for (slot, &block) in payload.iter_mut().zip(&blocks[..block_count]) {
                let start = (usize::from(block) + 1) * BLOCK_SIZE;
                *slot = Some(&bytes[start..start + BLOCK_SIZE]);
            }
            saves.push(Save {
                native_id: directory.filename(),
                blocks: payload,
                block_count: block_count as u8,
                file_size: directory.filesize() as usize,
            });
        }

        for (block, directory) in info.directory_frames().enumerate() {
            if matches!(
                directory.state(),
                Some(
                    AllocationState::AllocFirst
                        | AllocationState::AllocMid
                        | AllocationState::AllocLast
                )
            ) && !claimed[block]
            {
                return Err(ParseError::Malformed(
                    "allocated block is not part of a save chain",
                ));
            }
        }
        Ok(saves)
    }

    fn append_save(&mut self, save: Self::Save<'_>) -> Result<(), Self::Error> {
        let first_block = self.next_block;
        let block_count = save.block_count();
        if first_block + block_count > self.capacity_blocks {
            return Err(ParseError::Full);
        }
        let bytes = self.bytes.as_mut_slice().ok_or(ParseError::ReadOnly)?;
        for (chain_index, payload) in save.blocks().enumerate() {
            let block = first_block + chain_index;
            let data_start = (block + 1) * BLOCK_SIZE;
            bytes[data_start..data_start + BLOCK_SIZE].fill(0xff);
            bytes[data_start..data_start + payload.len()].copy_from_slice(payload);

            let frame = &mut bytes[(block + 1) * FRAME_SIZE..(block + 2) * FRAME_SIZE];
            frame.fill(0);
            let state = if chain_index == 0 {
                AllocationState::AllocFirst
            } else if chain_index + 1 == block_count {
                AllocationState::AllocLast
            } else {
                AllocationState::AllocMid
            };
            frame[..4].copy_from_slice(&state.raw().to_le_bytes());
            if chain_index == 0 {
                frame[4..8].copy_from_slice(&(save.file_size as u32).to_le_bytes());
                frame[10..31].copy_from_slice(save.native_id);
            }
            let next = if chain_index + 1 == block_count {
                u16::MAX
            } else {
                (block + 1) as u16
            };
            frame[8..10].copy_from_slice(&next.to_le_bytes());
            Self::update_checksum(frame);
        }
        self.next_block += block_count;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParseError {
    InvalidCapacity(u32),
    InvalidCardSize(usize),
    InvalidBlockSize(usize),
    InvalidCardHeader([u8; 2]),
    InvalidDataBlockHeader([u8; 2]),
    InvalidChecksum(usize),
    InvalidLayout(&'static str),
    Malformed(&'static str),
    ReadOnly,
    Full,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity(blocks) => write!(
                formatter,
                "PlayStation memory cards support 1 through {DATA_BLOCKS} blocks, not {blocks}"
            ),
            Self::InvalidCardSize(size) => {
                write!(formatter, "raw image is {size} bytes instead of {RAW_SIZE}")
            }
            Self::InvalidBlockSize(size) => {
                write!(
                    formatter,
                    "data block is {size} bytes instead of {BLOCK_SIZE}"
                )
            }
            Self::InvalidCardHeader(header) => {
                write!(formatter, "raw image has invalid card header {header:02x?}")
            }
            Self::InvalidDataBlockHeader(header) => {
                write!(formatter, "save data has invalid header {header:02x?}")
            }
            Self::InvalidChecksum(frame) => {
                write!(
                    formatter,
                    "memory-card frame {frame} has an invalid checksum"
                )
            }
            Self::InvalidLayout(name) => write!(formatter, "{name} has an invalid byte layout"),
            Self::Malformed(reason) => formatter.write_str(reason),
            Self::ReadOnly => formatter.write_str("memory-card view is read-only"),
            Self::Full => formatter.write_str("memory card is full"),
        }
    }
}
