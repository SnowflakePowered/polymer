use crate::{
    Error,
    keys::{KeyIdentity, KeyRecord},
};
use zerocopy::byteorder::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SeedDbHeader {
    count: U32,
    reserved: [u8; 12],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SeedDbEntry {
    title_id: U64,
    seed: [u8; 16],
    reserved: [u8; 8],
}

pub struct CtrSeedDb;

impl CtrSeedDb {
    pub fn decode(bytes: &[u8]) -> Result<Vec<KeyRecord>, Error> {
        let (header, entries) = SeedDbHeader::ref_from_prefix(bytes)
            .map_err(|_| Error::malformed("seeddb.bin", "truncated header"))?;
        let entries = <[SeedDbEntry]>::ref_from_bytes(entries)
            .map_err(|_| Error::malformed("seeddb.bin", "truncated seed entries"))?;
        if entries.is_empty() || entries.len() != header.count.get() as usize {
            return Err(Error::malformed(
                "seeddb.bin",
                "seed count does not match file size",
            ));
        }
        let mut records: Vec<KeyRecord> = entries
            .iter()
            .map(|entry| {
                let id = format!("{:016X}", entry.title_id.get());
                KeyRecord {
                    identity: KeyIdentity {
                        namespace: "system".into(),
                        id: id.clone(),
                    },
                    title_id: Some(id),
                    bytes: entry.seed.to_vec(),
                }
            })
            .collect();
        records.sort_by(|a, b| a.identity.id.cmp(&b.identity.id));
        records.dedup_by(|a, b| a.identity == b.identity);
        Ok(records)
    }
}
