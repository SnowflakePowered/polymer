use std::{io::Read, path::Path};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::big_endian::U16};

use super::tmd::Tmd;
use crate::impls::wii::index::SharedContentMap;
use crate::installable::{DepotFile, DepotType, IconHint, artifacts::Artifact};
use crate::{Error, VolumeStorage};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Imet {
    header: [u8; 0x1c],
    names: [[U16; 42]; 10],
}

pub(super) struct InstalledTitle;

impl InstalledTitle {
    pub fn read(
        storage: &dyn VolumeStorage,
        root: &str,
        title: u64,
        kind: DepotType,
        manifest: Vec<DepotFile>,
    ) -> Result<Artifact, Error> {
        let path = format!("{root}/title.tmd");
        let mut bytes = Vec::new();
        storage
            .open_read(&path)?
            .read_to_end(&mut bytes)
            .map_err(|e| Error::io(Path::new(&path), e))?;
        let tmd = Tmd::parse(&bytes, 0, u64::MAX)?;
        if tmd.title_id != title {
            return Err(Error::malformed(path, "installed Wii identity changed"));
        }
        let mut label = format!("{title:016x}");
        if let Some(content) = tmd
            .contents
            .iter()
            .find(|content| content.index == tmd.boot_index)
        {
            let path = if content.is_shared() {
                SharedContentMap::load(storage)?
                    .find(&content.sha1)
                    .map(|entry| format!("shared1/{}.app", entry.id))
            } else {
                Some(format!("{root}/{:08x}.app", content.id))
            };
            if let Some(path) = path {
                if let Some(name) = Self::name(storage, &path)? {
                    label = name;
                }
            }
        }
        // TODO: Gumshoe does not yet expose Wii channel banner decoding for installed content.
        Ok(Artifact::new(
            format!("{title:016x}"),
            kind,
            manifest,
            label,
            Some(tmd.title_version.to_string()),
            IconHint::Builtin(kind),
        ))
    }

    fn name(storage: &dyn VolumeStorage, path: &str) -> Result<Option<String>, Error> {
        let mut bytes = Vec::new();
        storage
            .open_read(path)?
            .take(0x640)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::io(Path::new(path), e))?;
        let Some(offset) = bytes.windows(4).position(|window| window == b"IMET") else {
            return Ok(None);
        };
        let Ok((imet, _)) = Imet::ref_from_prefix(&bytes[offset..]) else {
            return Ok(None);
        };
        Ok(std::iter::once(&imet.names[1])
            .chain(imet.names.iter())
            .find_map(|name| {
                let units = name
                    .iter()
                    .map(|unit| unit.get())
                    .take_while(|unit| *unit != 0)
                    .collect::<Vec<_>>();
                String::from_utf16(&units)
                    .ok()
                    .filter(|name| !name.trim().is_empty())
            }))
    }
}
