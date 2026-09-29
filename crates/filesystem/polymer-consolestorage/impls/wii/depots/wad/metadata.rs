use std::{io::Read, path::Path};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::big_endian::U16};

use super::tmd::Tmd;
use crate::impls::wii::index::SharedContentMap;
use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};
use crate::installable::{DepotFile, DepotType, EmbeddedIcon, IconHint};
use crate::{Error, VolumeStorage};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Imet {
    header: [u8; 0x1c],
    names: [[U16; 42]; 10],
}

pub(super) struct InstalledTitle;

impl InstalledTitle {
    const IMET_SEARCH_SIZE: u64 = 0x640;
}

impl InstalledTitle {
    pub fn read(
        storage: &dyn VolumeStorage,
        root: &str,
        title: u64,
        kind: DepotType,
        manifest: Vec<DepotFile>,
        icon: Option<EmbeddedIcon>,
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
        // Channel names live in the IMET header of the banner, which is always content 0.
        if let Some(content) = tmd.contents.iter().find(|content| content.index == 0) {
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
        let artifact = Artifact::new(
            format!("{title:016x}"),
            kind,
            manifest,
            label,
            Some(tmd.title_version.to_string()),
            vec![icon.map_or(IconHint::Builtin(kind), IconHint::Embedded)],
        );
        let boot = tmd
            .contents
            .iter()
            .find(|content| content.index == tmd.boot_index && !content.is_shared())
            .filter(|_| kind == DepotType::App);
        match boot {
            Some(boot) => artifact.with_entrypoint(ArtifactEntrypoint {
                root: root.to_owned(),
                path: format!("{:08x}.app", boot.id),
            }),
            None => Ok(artifact),
        }
    }

    fn name(storage: &dyn VolumeStorage, path: &str) -> Result<Option<String>, Error> {
        let mut bytes = Vec::new();
        storage
            .open_read(path)?
            .take(Self::IMET_SEARCH_SIZE)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::io(Path::new(path), e))?;
        Ok(Self::channel_name(&bytes))
    }

    /// The English channel name from the IMET header of a channel's opening
    /// banner (content 0), falling back to the first named language.
    pub(super) fn channel_name(opening: &[u8]) -> Option<String> {
        let header = &opening[..opening.len().min(Self::IMET_SEARCH_SIZE as usize)];
        let offset = header.windows(4).position(|window| window == b"IMET")?;
        let (imet, _) = Imet::ref_from_prefix(&header[offset..]).ok()?;
        std::iter::once(&imet.names[1])
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
            })
    }
}
