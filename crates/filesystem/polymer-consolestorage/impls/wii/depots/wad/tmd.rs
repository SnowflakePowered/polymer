use zerocopy::byteorder::big_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::signed_body;
use crate::Error;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct TmdHeader {
    before_title_id: [u8; 0x4c],
    title_id: U64,
    before_title_version: [u8; 0x48],
    title_version: U16,
    content_count: U16,
    boot_index: U16,
    reserved: [u8; 2],
}

const _: () = assert!(size_of::<TmdHeader>() == 0xa4);

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct TmdContent {
    id: U32,
    index: U16,
    kind: U16,
    size: U64,
    sha1: [u8; 20],
}

const _: () = assert!(size_of::<TmdContent>() == 0x24);

#[derive(Clone, Debug)]
pub(super) struct Content {
    pub(super) id: u32,
    pub(super) index: u16,
    pub(super) kind: u16,
    pub(super) size: u64,
    pub(super) sha1: [u8; 20],
    pub(super) wad_offset: u64,
}

impl Content {
    pub(super) fn is_shared(&self) -> bool {
        self.kind & 0x8000 != 0
    }
}

#[derive(Clone)]
pub(super) struct Tmd {
    pub(super) title_id: u64,
    pub(super) title_version: u16,
    pub(super) contents: Vec<Content>,
}

impl Tmd {
    pub(super) fn parse(bytes: &[u8], data_offset: u64, data_size: u64) -> Result<Self, Error> {
        let (_, body) = signed_body(bytes, "TMD")?;
        let (header, records) = TmdHeader::ref_from_prefix(body)
            .map_err(|_| Error::malformed("TMD", "TMD header is truncated"))?;
        let records_size = usize::from(header.content_count.get())
            .checked_mul(size_of::<TmdContent>())
            .ok_or_else(|| Error::malformed("TMD", "content table size overflows"))?;
        let records = records
            .get(..records_size)
            .ok_or_else(|| Error::malformed("TMD", "content table is truncated"))?;
        let data_end = data_offset
            .checked_add(data_size)
            .ok_or_else(|| Error::malformed("WAD", "data section overflows"))?;
        let mut wad_offset = data_offset;
        let mut contents = Vec::with_capacity(usize::from(header.content_count.get()));
        for record in records.chunks_exact(size_of::<TmdContent>()) {
            let record = TmdContent::ref_from_bytes(record)
                .map_err(|_| Error::malformed("TMD", "invalid content record"))?;
            let size = record.size.get();
            let end = wad_offset
                .checked_add(align(size, 0x40)?)
                .ok_or_else(|| Error::malformed("WAD", "content range overflows"))?;
            if end > data_end {
                return Err(Error::malformed("WAD", "content exceeds the data section"));
            }
            contents.push(Content {
                id: record.id.get(),
                index: record.index.get(),
                kind: record.kind.get(),
                size,
                sha1: record.sha1,
                wad_offset,
            });
            wad_offset = end;
        }
        Ok(Self {
            title_id: header.title_id.get(),
            title_version: header.title_version.get(),
            contents,
        })
    }
}

pub(super) fn align(value: u64, alignment: u64) -> Result<u64, Error> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| Error::malformed("WAD", "aligned size overflows"))
}
