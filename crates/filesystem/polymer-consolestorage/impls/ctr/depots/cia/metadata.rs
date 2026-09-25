use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use zerocopy::byteorder::big_endian as be;
use zerocopy::byteorder::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

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
    partition_id: U64,
    maker_code: [u8; 2],
    version: U16,
    before_flags: [u8; 0x74],
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

const NCCH_KEYX_2C: u128 = 0xb98e95ceca3e4d171f76a94de934c053;
const AES_GENERATOR: u128 = 0x1ff9e9aac5fe0408024591dc5d52768a;
const NO_CRYPTO: u8 = 0x04;
const FIXED_KEY: u8 = 0x01;
const EXEFS_REGION: u8 = 2;
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
        let header = NcchHeader::read_from_bytes(&bytes).expect("fixed NCCH header");
        if header.magic != *b"NCCH" {
            return Ok(None);
        }
        let Some((offset, size)) = header.exefs_range(length) else {
            return Ok(None);
        };
        reader
            .seek(SeekFrom::Start(offset))
            .and_then(|_| reader.read_exact(&mut bytes))
            .map_err(|error| Error::io(Path::new(path), error))?;
        header.decrypt_exefs(offset, 0, &mut bytes);
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
            header.decrypt_exefs(offset, start, &mut icon);
            return Ok(ExtdataIcon::parse(&icon));
        }
        Ok(None)
    }
}

impl NcchHeader {
    fn decrypt_exefs(&self, exefs_offset: u64, stream_offset: u64, bytes: &mut [u8]) {
        if self.flags[7] & NO_CRYPTO != 0 {
            return;
        }
        let key = if self.flags[7] & FIXED_KEY != 0 {
            0
        } else {
            let key_y =
                u128::from_be_bytes(self.signature[..0x10].try_into().expect("fixed signature"));
            ((NCCH_KEYX_2C.rotate_left(2) ^ key_y).wrapping_add(AES_GENERATOR)).rotate_left(87)
        };
        let partition = self.partition_id.get();
        let counter = match self.version.get() {
            1 => (u128::from(partition.swap_bytes()) << 64) | u128::from(exefs_offset as u32),
            _ => (u128::from(partition) << 64) | (u128::from(EXEFS_REGION) << 56),
        };
        let mut cipher =
            ctr::Ctr128BE::<Aes128>::new(&key.to_be_bytes().into(), &counter.to_be_bytes().into());
        cipher.seek(stream_offset);
        cipher.apply_keystream(bytes);
    }

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
