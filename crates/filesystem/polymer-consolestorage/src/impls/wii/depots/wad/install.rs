use std::{io::Cursor, path::PathBuf, sync::Arc};

use super::{WiiWadDepot, tmd::Content};
use crate::installable::{Depot, DepotFile, InstallContext, Installable, artifacts::Artifact};
use crate::{Error, Storage};

pub(super) struct WadInstall {
    source: WiiWadDepot,
    storage: Arc<dyn Storage>,
    key: [u8; 16],
    index: usize,
    root: String,
}

impl WadInstall {
    pub fn new(
        source: WiiWadDepot,
        storage: Arc<dyn Storage>,
        context: &InstallContext,
    ) -> Result<Self, Error> {
        context.phase(
            crate::installable::InstallPhase::Installing,
            source.tmd.contents.iter().map(|content| content.size).sum(),
        );
        Ok(Self {
            key: source.select_title_key()?,
            root: format!(
                "{}/content",
                crate::impls::wii::title_path(source.title_id())
            ),
            source,
            storage,
            index: 0,
        })
    }

    async fn content(
        &mut self,
        content: &Content,
        context: &InstallContext,
    ) -> Result<DepotFile, Error> {
        let bytes = self.source.decrypt_content(content, &self.key)?;
        let path = format!("{}/{:08x}.app", self.root, content.id);
        let copied = self
            .storage
            .install_file(Box::new(Cursor::new(bytes)), &path)
            .await?;
        context.advance(content.size);
        Ok(DepotFile::new(
            PathBuf::from(path),
            copied.blake3,
            copied.byte_length,
        ))
    }
}

impl Installable for WadInstall {
    async fn next_batch(
        &mut self,
        context: &InstallContext,
    ) -> Result<Option<Vec<DepotFile>>, Error> {
        if let Some(content) = self.source.tmd.contents.get(self.index).cloned() {
            let file = self.content(&content, context).await?;
            self.index += 1;
            return Ok(Some(vec![file]));
        }
        if self.index > self.source.tmd.contents.len() {
            return Ok(None);
        }
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
        super::metadata::InstalledTitle::read(
            &*self.storage,
            &self.root,
            self.source.title_id(),
            self.source.depot_type(),
            manifest,
            self.source.channel().icon.clone(),
        )
    }
}
