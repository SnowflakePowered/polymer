use std::collections::BTreeMap;

use futures::StreamExt;

use super::TitleId;
use crate::Error;
use crate::savegame::{SaveBlob, SaveBlobStream};

/// Validate the complete family before replacing any of its live files.
pub(super) struct SaveImport {
    pub title: TitleId,
    pub files: BTreeMap<String, SaveBlob>,
}

impl SaveImport {
    pub async fn read(mut blobs: SaveBlobStream<'_>, save_root: &str) -> Result<Self, Error> {
        let mut title = None;
        let mut files = BTreeMap::new();
        while let Some(blob) = blobs.next().await {
            let (path, candidate) = Self::validate(&blob, save_root)?;
            if title.is_some_and(|title| title != candidate) {
                return Err(Error::InvalidPath(
                    "a PS3 save depot cannot contain multiple TITLE_ID families".to_owned(),
                ));
            }
            title = Some(candidate);
            if files.insert(path.clone(), blob).is_some() {
                return Err(Error::InvalidPath(format!(
                    "duplicate PS3 save file `{path}`"
                )));
            }
        }
        for path in files.keys() {
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if files.contains_key(parent) {
                    return Err(Error::InvalidPath(format!(
                        "PS3 save file `{parent}` is also used as a directory"
                    )));
                }
                ancestor = parent;
            }
        }
        Ok(Self {
            title: title
                .ok_or_else(|| Error::InvalidPath("a PS3 save depot is empty".to_owned()))?,
            files,
        })
    }

    fn validate(blob: &SaveBlob, save_root: &str) -> Result<(String, TitleId), Error> {
        let path = blob
            .target_path
            .to_str()
            .ok_or_else(|| Error::InvalidPath("PS3 save path is not valid UTF-8".to_owned()))?
            .replace('\\', "/");
        if path.split('/').any(|component| {
            matches!(component, "" | "." | ".." | ".git")
                || component.contains([':', '\0', '<', '>', '"', '|', '?', '*'])
        }) {
            return Err(Error::InvalidPath(path));
        }
        let relative = path
            .strip_prefix(save_root)
            .and_then(|relative| relative.strip_prefix('/'))
            .ok_or_else(|| Error::InvalidPath(path.clone()))?;
        let (directory, _) = relative
            .split_once('/')
            .ok_or_else(|| Error::InvalidPath(path.clone()))?;
        let title = TitleId::from_directory(directory)
            .ok_or_else(|| Error::InvalidPath(format!("`{directory}` has no PS3 title ID")))?;
        if *blake3::hash(&blob.bytes).as_bytes() != blob.blake3 {
            return Err(Error::IntegrityMatchFailed { failed: path });
        }
        Ok((path, title))
    }
}
