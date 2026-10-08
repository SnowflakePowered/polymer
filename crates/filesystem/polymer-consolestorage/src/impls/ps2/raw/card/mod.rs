use std::borrow::Cow;
use std::fmt;
use std::io::{Cursor, Write};

use encoding_rs::SHIFT_JIS;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{U16, U32, U64},
};

use super::super::psu::{self, Encoder, Save};
use crate::impls::memorycard::{CardBytes, RawMemoryCardImage};

mod directory;
mod error;
mod format;
mod geometry;
mod image;
mod superblock;
mod write;

use directory::*;
pub(in crate::impls::ps2) use error::ParseError;
use geometry::*;
pub(in crate::impls::ps2) use image::MemoryCard;
use superblock::*;

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
