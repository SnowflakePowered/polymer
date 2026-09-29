use std::{io::Cursor, path::PathBuf, sync::Arc};

use super::{WiiWadDepot, tmd::Content};
use crate::impls::wii::index::{SharedContentMap, UidMap};
use crate::installable::{Depot, DepotFile, Installable, artifacts::Artifact};
use crate::{Error, VolumeStorage};

pub(super) struct WadInstall {
    source: WiiWadDepot,
    storage: Arc<dyn VolumeStorage>,
    key: [u8; 16],
    shared: SharedContentMap,
    index: usize,
    old: Option<Artifact>,
    root: String,
    events: crate::installable::InstallEvents,
}

impl WadInstall {
    pub async fn new(
        source: WiiWadDepot,
        storage: Arc<dyn VolumeStorage>,
        events: crate::installable::InstallEvents,
    ) -> Result<Self, Error> {
        let old = storage
            .artifacts()
            .artifact(&format!("{:016x}", source.title_id()))
            .await?;
        events.phase(
            crate::installable::InstallPhase::Installing,
            source.tmd.contents.iter().map(|content| content.size).sum(),
        );
        Ok(Self {
            events,
            key: source.select_title_key()?,
            shared: SharedContentMap::load(&*storage)?,
            root: format!(
                "{}/content",
                crate::impls::wii::title_path(source.title_id())
            ),
            source,
            storage,
            old,
            index: 0,
        })
    }

    async fn content(&mut self, content: &Content) -> Result<DepotFile, Error> {
        let bytes = self.source.decrypt_content(content, &self.key)?;
        let mut changed = false;
        let path = if content.is_shared() {
            let id = match self.shared.find(&content.sha1) {
                Some(entry) => entry.id.clone(),
                None => {
                    let id = self.shared.next_id()?;
                    self.shared.add(id.clone(), content.sha1);
                    changed = true;
                    id
                }
            };
            format!("shared1/{id}.app")
        } else {
            format!("{}/{:08x}.app", self.root, content.id)
        };
        let copied = if content.is_shared()
            && self.storage.exists(&path)
            && SharedContentMap::file_matches(&*self.storage, &path, &content.sha1)
        {
            crate::InstallResult {
                blake3: *blake3::hash(&bytes).as_bytes(),
                byte_length: bytes.len() as u64,
            }
        } else {
            self.storage
                .install_file(Box::new(Cursor::new(bytes)), &path)
                .await?
        };
        if changed {
            self.shared.write(&*self.storage).await?;
        }
        self.events.advance(content.size);
        Ok(DepotFile::new(
            PathBuf::from(path),
            copied.blake3,
            copied.byte_length,
        ))
    }
}

impl Installable for WadInstall {
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error> {
        if let Some(content) = self.source.tmd.contents.get(self.index).cloned() {
            let file = self.content(&content).await?;
            self.index += 1;
            return Ok(Some(vec![file]));
        }
        if self.index > self.source.tmd.contents.len() {
            return Ok(None);
        }
        UidMap::load(&*self.storage)?
            .insert(&*self.storage, self.source.title_id())
            .await?;
        let path = format!("{}/title.tmd", self.root);
        let copied = self
            .storage
            .install_file(Box::new(Cursor::new(self.source.read_tmd()?)), &path)
            .await?;
        self.index += 1;
        Ok(Some(vec![DepotFile::new(
            path.into(),
            copied.blake3,
            copied.byte_length,
        )]))
    }

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        let artifact = super::metadata::InstalledTitle::read(
            &*self.storage,
            &self.root,
            self.source.title_id(),
            self.source.depot_type(),
            manifest,
            self.source.channel().icon.clone(),
        )?;
        if let Some(old) = self.old {
            for file in old.manifest() {
                let path = file.path().to_string_lossy();
                if path.starts_with(&format!("{}/", self.root))
                    && !artifact
                        .manifest()
                        .iter()
                        .any(|current| current.path() == file.path())
                {
                    self.storage.remove(&path)?;
                }
            }
        }
        Ok(artifact)
    }
}
