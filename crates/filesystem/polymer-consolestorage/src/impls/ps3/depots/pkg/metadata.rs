use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use zerocopy::byteorder::big_endian::U32;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::Error;
use crate::installable::DepotType;

// PKG metadata is not exposed by pkgrip. This is the unencrypted prefix and
// packet layout documented at https://www.psdevwiki.com/ps3/PKG_files.
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Prefix {
    magic: U32,
    revision_and_type: U32,
    metadata_offset: U32,
    metadata_count: U32,
    metadata_size: U32,
    item_count: U32,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Packet {
    kind: U32,
    size: U32,
}

const _: () = assert!(size_of::<Prefix>() == 0x18 && size_of::<Packet>() == 8);

#[derive(Default)]
pub(super) struct PackageMetadata {
    content_type: u32,
    flags: u32,
    pub version: Option<String>,
    pub directory: Option<String>,
}

impl PackageMetadata {
    pub fn read(file: &mut File) -> Result<Self, Error> {
        let mut bytes = [0; size_of::<Prefix>()];
        file.read_exact(&mut bytes)
            .map_err(|error| Error::malformed("PKG metadata", error.to_string()))?;
        let prefix = Prefix::ref_from_bytes(&bytes).unwrap();
        let size = prefix.metadata_size.get() as usize;
        if size > 1024 * 1024 || prefix.metadata_count.get() as usize > size / 8 {
            return Err(Error::malformed(
                "PKG metadata",
                "invalid packet table size",
            ));
        }
        let mut bytes = vec![0; size];
        file.seek(SeekFrom::Start(prefix.metadata_offset.get().into()))
            .and_then(|_| file.read_exact(&mut bytes))
            .map_err(|error| Error::malformed("PKG metadata", error.to_string()))?;
        let mut remaining = bytes.as_slice();
        let mut metadata = Self::default();
        for _ in 0..prefix.metadata_count.get() {
            let (packet, tail) = Packet::ref_from_prefix(remaining)
                .map_err(|_| Error::malformed("PKG metadata", "truncated packet"))?;
            let size = packet.size.get() as usize;
            let data = tail
                .get(..size)
                .ok_or_else(|| Error::malformed("PKG metadata", "truncated packet data"))?;
            metadata.apply(packet.kind.get(), data)?;
            remaining = &tail[size..];
        }
        Ok(metadata)
    }

    fn apply(&mut self, kind: u32, data: &[u8]) -> Result<(), Error> {
        match kind {
            2 | 3 => {
                let value = U32::read_from_bytes(data)
                    .map_err(|_| Error::malformed("PKG metadata", "expected a four-byte value"))?
                    .get();
                if kind == 2 {
                    self.content_type = value;
                } else {
                    self.flags = value;
                }
            }
            8 if data.len() == 8 => {
                self.version = Some(format!("{:02x}.{:02x}", data[4], data[5]));
            }
            0xa => {
                let bytes = data
                    .get(8..)
                    .ok_or_else(|| Error::malformed("PKG metadata", "invalid install directory"))?;
                let end = bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(bytes.len());
                let directory = std::str::from_utf8(&bytes[..end]).map_err(|_| {
                    Error::malformed("PKG metadata", "invalid install directory encoding")
                })?;
                if !directory.is_empty() {
                    self.directory = Some(directory.to_owned());
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn depot_type(&self) -> Result<DepotType, Error> {
        match self.content_type {
            4 | 5 if self.flags & 0x10 != 0 => Ok(DepotType::AppUpdate),
            4 => Ok(DepotType::AppContent),
            5 => Ok(DepotType::App),
            other => Err(Error::Unsupported(format!(
                "PS3 PKG content type {other:#x}"
            ))),
        }
    }
}
