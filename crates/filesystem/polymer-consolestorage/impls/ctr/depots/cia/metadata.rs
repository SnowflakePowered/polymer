use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use zerocopy::byteorder::big_endian as be;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::U32};

use super::CiaDepot;
use crate::impls::ctr::smdh::ExtdataIcon;
use crate::installable::{DepotFile, IconHint, artifacts::Artifact};
use crate::{Error, VolumeStorage};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct NcchHeader {
    signature: [u8; 0x100],
    magic: [u8; 4],
    size: U32,
    before_flags: [u8; 0x80],
    flags: [u8; 8],
    before_exefs: [u8; 0x10],
    exefs_offset: U32,
    exefs_size: U32,
    rest: [u8; 0x58],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ExeFsEntry {
    name: [u8; 8],
    offset: U32,
    size: U32,
}

const _: () = assert!(size_of::<NcchHeader>() == 0x200);
const _: () = assert!(size_of::<ExeFsEntry>() == 0x10);

pub(super) struct InstalledTitle;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct TmdHeader {
    before_title: [u8; 0x4c],
    title_id: be::U64,
    before_version: [u8; 0x48],
    version: be::U16,
    count: be::U16,
    boot_index: be::U16,
    padding: [u8; 2],
    info_hash: [u8; 32],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct TmdContent {
    id: be::U32,
    index: be::U16,
    kind: be::U16,
    size: be::U64,
    hash: [u8; 32],
}

const _: () = assert!(size_of::<TmdHeader>() == 0xc4);
const _: () = assert!(size_of::<TmdContent>() == 0x30);

impl TmdHeader {
    fn parse(bytes: &[u8]) -> Result<(&Self, &[TmdContent]), Error> {
        let invalid = || Error::malformed("installed TMD", "invalid or truncated title metadata");
        let signature = u32::from_be_bytes(*bytes.first_chunk().ok_or_else(invalid)?);
        let offset = match signature {
            0x10000 | 0x10003 => 0x240,
            0x10001 | 0x10004 => 0x140,
            0x10002 | 0x10005 => 0x80,
            _ => return Err(invalid()),
        };
        let (header, rest) = Self::ref_from_prefix(bytes.get(offset..).ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        let length = usize::from(header.count.get()) * size_of::<TmdContent>();
        let records = rest.get(0x900..0x900 + length).ok_or_else(invalid)?;
        Ok((
            header,
            <[TmdContent]>::ref_from_bytes(records).map_err(|_| invalid())?,
        ))
    }
}

impl InstalledTitle {
    pub fn read(
        depot: &CiaDepot,
        storage: &dyn VolumeStorage,
        manifest: Vec<DepotFile>,
    ) -> Result<Artifact, Error> {
        let id = format!("{:016X}", depot.title_id);
        let path = format!("{}/00000000.tmd", depot.title_root);
        let mut bytes = Vec::new();
        storage
            .open_read(&path)?
            .take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::io(Path::new(&path), error))?;
        let (tmd, contents) = TmdHeader::parse(&bytes)?;
        if tmd.title_id.get() != depot.title_id {
            return Err(Error::malformed(&path, "installed CIA title ID changed"));
        }
        let kind = CiaDepot::title_kind(tmd.title_id.get());
        let mut label = id.clone();
        let mut icon = IconHint::Builtin(kind);
        if let Some(content) = contents
            .iter()
            .find(|content| content.index.get() == tmd.boot_index.get())
        {
            let root = if contents
                .get(1)
                .is_some_and(|content| content.kind.get() & 0x4000 != 0)
            {
                format!("{}/00000000", depot.title_root)
            } else {
                depot.title_root.clone()
            };
            let path = format!("{root}/{:08x}.app", content.id.get());
            if manifest.iter().any(|file| file.path() == Path::new(&path)) {
                if let Some(smdh) = Self::smdh(storage, &path)? {
                    let name = smdh.description().0;
                    if !name.trim().is_empty() {
                        label = name;
                    }
                    icon = IconHint::image(smdh.rgba())?;
                }
            }
        }
        Ok(Artifact::new(
            id,
            kind,
            manifest,
            label,
            Some(tmd.version.get().to_string()),
            icon,
        ))
    }

    fn smdh(storage: &dyn VolumeStorage, path: &str) -> Result<Option<ExtdataIcon>, Error> {
        let length = storage.metadata(path)?.len;
        if length < 0x200 {
            return Ok(None);
        }
        let mut reader = storage.open_read(path)?;
        let mut bytes = [0; 0x200];
        reader
            .read_exact(&mut bytes)
            .map_err(|error| Error::io(Path::new(path), error))?;
        let header = NcchHeader::ref_from_bytes(&bytes).expect("fixed NCCH header");
        if header.magic != *b"NCCH" || header.flags[7] & 4 == 0 {
            // TODO: use Gumshoe when its public readers support installed encrypted NCCH.
            return Ok(None);
        }
        let Some((offset, size)) = header.exefs_range(length) else {
            return Ok(None);
        };
        reader
            .seek(SeekFrom::Start(offset))
            .and_then(|_| reader.read_exact(&mut bytes))
            .map_err(|error| Error::io(Path::new(path), error))?;
        for bytes in bytes[..0xa0].chunks_exact(0x10) {
            let entry = ExeFsEntry::ref_from_bytes(bytes).expect("fixed ExeFS entry");
            if entry.name != *b"icon\0\0\0\0" {
                continue;
            }
            let start = 0x200 + u64::from(entry.offset.get());
            if u64::from(entry.size.get()) < 0x36c0 || start + u64::from(entry.size.get()) > size {
                return Ok(None);
            }
            let mut icon = vec![0; 0x36c0];
            reader
                .seek(SeekFrom::Start(offset + start))
                .and_then(|_| reader.read_exact(&mut icon))
                .map_err(|error| Error::io(Path::new(path), error))?;
            return Ok(ExtdataIcon::parse(&icon));
        }
        Ok(None)
    }
}

impl NcchHeader {
    fn exefs_range(&self, length: u64) -> Option<(u64, u64)> {
        let unit = 0x200_u64.checked_shl(u32::from(self.flags[6]))?;
        let content_size = u64::from(self.size.get()).checked_mul(unit)?;
        let offset = u64::from(self.exefs_offset.get()).checked_mul(unit)?;
        let size = u64::from(self.exefs_size.get()).checked_mul(unit)?;
        (offset >= 0x200
            && size >= 0x200
            && offset.checked_add(size)? <= content_size
            && content_size <= length)
            .then_some((offset, size))
    }
}
