use std::collections::BTreeMap;

use futures::StreamExt;

use super::layout::Archive;
use crate::{Error, savegame::SaveBlobStream};

pub(super) struct SaveImport {
    pub archive: Archive,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl SaveImport {
    pub async fn read(mut blobs: SaveBlobStream<'_>) -> Result<Self, Error> {
        let mut archive = None;
        let mut files = BTreeMap::new();
        while let Some(blob) = blobs.next().await {
            let path = blob
                .target_path
                .to_str()
                .ok_or_else(|| Error::InvalidPath("3DS save path is not UTF-8".to_owned()))?
                .replace('\\', "/");
            let candidate =
                Archive::from_path(&path).ok_or_else(|| Error::InvalidPath(path.clone()))?;
            if archive.is_some_and(|current| current != candidate) {
                return Err(Error::InvalidPath(
                    "3DS save import mixes archives".to_owned(),
                ));
            }
            archive = Some(candidate);
            if *blake3::hash(&blob.bytes).as_bytes() != blob.blake3 {
                return Err(Error::IntegrityMatchFailed { failed: path });
            }
            if files.insert(path.clone(), blob.bytes).is_some() {
                return Err(Error::InvalidPath(format!(
                    "duplicate 3DS save path {path}"
                )));
            }
        }
        let archive =
            archive.ok_or_else(|| Error::InvalidPath("empty 3DS save import".to_owned()))?;
        Self::validate_paths(archive, &files)?;
        Ok(Self { archive, files })
    }

    fn validate_paths(archive: Archive, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
        let mut folded = std::collections::BTreeSet::new();
        for path in files.keys() {
            if !path
                .to_ascii_lowercase()
                .starts_with(&format!("{}/", archive.root()).to_ascii_lowercase())
                || !folded.insert(path.to_ascii_lowercase())
            {
                return Err(Error::InvalidPath(path.clone()));
            }
        }
        for path in &folded {
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if folded.contains(parent) {
                    return Err(Error::InvalidPath(format!(
                        "save file is also a directory: {parent}"
                    )));
                }
                ancestor = parent;
            }
        }
        Ok(())
    }
}
