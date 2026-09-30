use std::io::Read;
use std::path::Path;

use zerocopy::byteorder::big_endian as be;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::CiaDepot;
use super::ncch::NcchHeader;
use crate::impls::ctr::smdh::ExtdataIcon;
use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};
use crate::installable::{DepotFile, DepotType, EmbeddedIcon, IconHint};
use crate::{Error, VolumeStorage};
use polymer_banner::ctr::CtrBanner;

pub(super) struct InstalledTitle;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct TmdHeader {
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
pub(super) struct TmdContent {
    pub(super) id: be::U32,
    index: be::U16,
    kind: be::U16,
    pub(super) size: be::U64,
    pub(super) hash: [u8; 32],
}

const _: () = assert!(size_of::<TmdHeader>() == 0xc4);
const _: () = assert!(size_of::<TmdContent>() == 0x30);

impl TmdHeader {
    pub(super) fn parse(bytes: &[u8]) -> Result<(&Self, &[TmdContent]), Error> {
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
        let bytes = Self::tmd(depot, storage)?;
        let (tmd, contents) = TmdHeader::parse(&bytes)?;
        if tmd.title_id.get() != depot.title_id {
            return Err(Error::malformed(&path, "installed CIA title ID changed"));
        }
        let kind = CiaDepot::title_kind(tmd.title_id.get());
        let mut label = id.clone();
        let mut icons = vec![IconHint::Builtin(kind)];
        let boot = contents
            .first()
            .map(|content| depot.content_path(content.id.get()));
        if let Some(path) = &boot {
            if manifest.iter().any(|file| file.path() == Path::new(path)) {
                let smdh = Self::exefs_file(storage, path, *b"icon\0\0\0\0")?
                    .and_then(|icon| ExtdataIcon::parse(icon.get(..size_of::<ExtdataIcon>())?));
                if let Some(smdh) = smdh {
                    let name = smdh.description().0;
                    if !name.trim().is_empty() {
                        label = name;
                    }
                    icons = vec![IconHint::image(smdh.rgba())?];
                }
                icons.extend(
                    Self::exefs_file(storage, path, *b"banner\0\0")?
                        .and_then(|banner| Some(CtrBanner::strip_sound(&banner)?.to_vec()))
                        .filter(|banner| CtrBanner::parse(banner).is_some())
                        .map(|banner| IconHint::Embedded(EmbeddedIcon::CtrBanner(banner.into()))),
                );
            }
        }
        let entrypoint = boot.filter(|_| kind == DepotType::App).and_then(|package| {
            Some(ArtifactEntrypoint {
                path: package
                    .strip_prefix(&depot.title_root)?
                    .strip_prefix('/')?
                    .to_owned(),
                root: depot.title_root.clone(),
                package,
            })
        });
        Artifact::new(
            id,
            kind,
            manifest,
            label,
            Some(tmd.version.get().to_string()),
            icons,
            entrypoint,
        )
    }

    pub(super) fn tmd(depot: &CiaDepot, storage: &dyn VolumeStorage) -> Result<Vec<u8>, Error> {
        let path = format!("{}/00000000.tmd", depot.title_root);
        let mut bytes = Vec::new();
        storage
            .open_read(&path)?
            .take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::io(Path::new(&path), error))?;
        Ok(bytes)
    }

    /// A decrypted ExeFS file of an installed NCCH content.
    fn exefs_file(
        storage: &dyn VolumeStorage,
        path: &str,
        name: [u8; 8],
    ) -> Result<Option<Vec<u8>>, Error> {
        let length = storage.metadata(path)?.len;
        NcchHeader::exefs_file(&mut storage.open_read(path)?, 0, length, name)
            .map_err(|error| Error::io(Path::new(path), error))
    }
}
