use std::collections::HashSet;
use std::io::{Cursor, Read};

use sha1::{Digest, Sha1};
use zerocopy::byteorder::big_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::{Error, Storage};

pub(crate) const INITIAL_UID_SYS: &[u8] = &[
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x10, 0x00,
];

#[derive(Clone)]
pub(crate) struct SharedEntry {
    pub(crate) id: String,
    pub(crate) sha1: [u8; 20],
}

pub(crate) struct SharedContentMap {
    entries: Vec<SharedEntry>,
}

impl SharedContentMap {
    pub(crate) fn load(storage: &dyn Storage) -> Result<Self, Error> {
        if !storage.exists("shared1/content.map") {
            return Ok(Self {
                entries: Vec::new(),
            });
        }
        let bytes = Self::read(storage, "shared1/content.map")?;
        if !bytes.len().is_multiple_of(28) {
            return Err(Error::malformed(
                "shared1/content.map",
                "length is not a multiple of 28",
            ));
        }
        let mut entries = Vec::with_capacity(bytes.len() / 28);
        for entry in bytes.chunks_exact(28) {
            let id = std::str::from_utf8(&entry[..8])
                .ok()
                .filter(|id| id.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .ok_or_else(|| {
                    Error::malformed(
                        "shared1/content.map",
                        "contains an invalid shared-content ID",
                    )
                })?
                .to_owned();
            let mut sha1 = [0; 20];
            sha1.copy_from_slice(&entry[8..]);
            entries.push(SharedEntry { id, sha1 });
        }
        Ok(Self { entries })
    }

    pub(crate) fn find(&self, sha1: &[u8; 20]) -> Option<&SharedEntry> {
        self.entries.iter().find(|entry| entry.sha1 == *sha1)
    }

    pub(crate) fn add(&mut self, id: String, sha1: [u8; 20]) {
        self.entries.push(SharedEntry { id, sha1 });
    }

    pub(crate) fn next_id(&self) -> Result<String, Error> {
        let last = self
            .entries
            .iter()
            .filter_map(|entry| u32::from_str_radix(&entry.id, 16).ok())
            .max();
        last.map_or(Some(0), |id| id.checked_add(1))
            .map(|id| format!("{id:08x}"))
            .ok_or_else(|| Error::malformed("shared1/content.map", "ID namespace is exhausted"))
    }

    pub(crate) fn is_valid(&self) -> bool {
        let ids = self
            .entries
            .iter()
            .map(|entry| &entry.id)
            .collect::<HashSet<_>>();
        let hashes = self
            .entries
            .iter()
            .map(|entry| entry.sha1)
            .collect::<HashSet<_>>();
        ids.len() == self.entries.len() && hashes.len() == self.entries.len()
    }

    pub(crate) fn file_matches(storage: &dyn Storage, path: &str, expected: &[u8; 20]) -> bool {
        Self::read(storage, path).is_ok_and(|bytes| Sha1::digest(bytes).as_slice() == expected)
    }

    pub(crate) async fn write(&self, storage: &dyn Storage) -> Result<(), Error> {
        let mut bytes = Vec::with_capacity(self.entries.len() * 28);
        for entry in &self.entries {
            bytes.extend_from_slice(entry.id.as_bytes());
            bytes.extend_from_slice(&entry.sha1);
        }
        storage
            .install_file(Box::new(Cursor::new(bytes)), "shared1/content.map")
            .await?;
        Ok(())
    }

    fn read(storage: &dyn Storage, path: &str) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        storage
            .open_read(path)?
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io {
                path: path.to_owned(),
                source,
            })?;
        Ok(bytes)
    }
}

pub(crate) struct UidMap {
    bytes: Vec<u8>,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct UidEntry {
    title_id: U64,
    uid: U32,
}

const _: () = assert!(size_of::<UidEntry>() == 12);

impl UidMap {
    pub(crate) fn load(storage: &dyn Storage) -> Result<Self, Error> {
        let mut bytes = Vec::new();
        if storage.exists("sys/uid.sys") {
            storage
                .open_read("sys/uid.sys")?
                .read_to_end(&mut bytes)
                .map_err(|source| Error::Io {
                    path: "sys/uid.sys".to_owned(),
                    source,
                })?;
        }
        if !bytes.len().is_multiple_of(12) {
            return Err(Error::malformed(
                "sys/uid.sys",
                "length is not a multiple of 12",
            ));
        }
        Ok(Self { bytes })
    }

    pub(crate) fn is_valid(&self) -> bool {
        let titles = self
            .bytes
            .chunks_exact(12)
            .filter_map(|entry| UidEntry::ref_from_bytes(entry).ok())
            .map(|entry| entry.title_id.get())
            .collect::<HashSet<_>>();
        let ids = self
            .bytes
            .chunks_exact(12)
            .filter_map(|entry| UidEntry::ref_from_bytes(entry).ok())
            .map(|entry| entry.uid.get())
            .collect::<HashSet<_>>();
        titles.len() == self.bytes.len() / 12
            && ids.len() == self.bytes.len() / 12
            && self.bytes.chunks_exact(12).any(|entry| {
                UidEntry::ref_from_bytes(entry).is_ok_and(|entry| {
                    entry.title_id.get() == 0x0000_0001_0000_0002 && entry.uid.get() == 0x1000
                })
            })
    }

    pub(crate) async fn insert(
        &mut self,
        storage: &dyn Storage,
        title_id: u64,
    ) -> Result<(), Error> {
        if self.bytes.is_empty() {
            self.bytes.extend_from_slice(INITIAL_UID_SYS);
        }
        if self
            .bytes
            .chunks_exact(12)
            .filter_map(|entry| UidEntry::ref_from_bytes(entry).ok())
            .any(|entry| entry.title_id.get() == title_id)
        {
            return Ok(());
        }
        let next = self
            .bytes
            .chunks_exact(12)
            .filter_map(|entry| UidEntry::ref_from_bytes(entry).ok())
            .map(|entry| entry.uid.get())
            .max()
            .map_or(0x1000, |uid| uid.saturating_add(1));
        self.bytes.extend_from_slice(&title_id.to_be_bytes());
        self.bytes.extend_from_slice(&next.to_be_bytes());
        storage
            .install_file(Box::new(Cursor::new(self.bytes.clone())), "sys/uid.sys")
            .await?;
        Ok(())
    }
}
