use tokio::io::{AsyncRead, AsyncReadExt};

use super::{AttachmentInfo, SaveAttachmentKind, SaveCommitId, SaveDepotId, SaveVersioning};
use crate::Error;

impl SaveVersioning {
    fn note_path(commit: SaveCommitId) -> String {
        let id = commit.to_string();
        format!("{}/{}", &id[..2], &id[2..])
    }

    pub(crate) fn attachment(
        &self,
        depot: SaveDepotId,
        commit: SaveCommitId,
        kind: SaveAttachmentKind,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.ensure_member(depot, commit)?;
        let repo = self.inner.repository().to_thread_local();
        let refs = self.refs(depot);
        let Some(notes_tip) = Self::tip_in(&repo, refs.attachment(kind))? else {
            return Ok(None);
        };
        let tree = Self::commit_tree(&repo, notes_tip)?;
        Ok(Self::tree_files(&repo, tree)?.remove(&Self::note_path(commit)))
    }

    pub(crate) async fn attach(
        &self,
        depot: SaveDepotId,
        commit: SaveCommitId,
        kind: SaveAttachmentKind,
        mut source: Box<dyn AsyncRead + Send + Unpin>,
    ) -> Result<AttachmentInfo, Error> {
        let mut bytes = Vec::new();
        source
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| Error::Io {
                path: format!("{} attachment", kind.as_str()),
                source: error,
            })?;
        if kind == SaveAttachmentKind::Note {
            std::str::from_utf8(&bytes).map_err(|error| {
                Error::HistoryConflict(format!("commit note is not UTF-8: {error}"))
            })?;
        }
        let _guard = self.inner.operation.lock().await;
        self.ensure_member(depot, commit)?;
        let repo = self.inner.repository().to_thread_local();
        let reference = self.refs(depot).attachment(kind).to_owned();
        let parent = Self::tip_in(&repo, &reference)?;
        let root = match parent {
            Some(parent) => repo
                .find_tree(Self::commit_tree(&repo, parent)?)
                .map_err(|error| Error::git("read notes tree", error))?,
            None => repo.empty_tree(),
        };
        let blob = repo
            .write_blob(&bytes)
            .map_err(|error| Error::git("write attachment", error))?
            .detach();
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit notes tree", error))?;
        editor
            .upsert(
                Self::note_path(commit),
                gix::objs::tree::EntryKind::Blob,
                blob,
            )
            .map_err(|error| Error::git("add note", error))?;
        let tree = editor
            .write()
            .map_err(|error| Error::git("write notes tree", error))?
            .detach();
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        repo.commit_as(
            signature,
            signature,
            reference,
            format!("Polymer {} for {commit}", kind.as_str()),
            tree,
            parent,
        )
        .map_err(|error| Error::git("commit attachment note", error))?;
        Ok(AttachmentInfo {
            object_id: SaveCommitId(blob),
            byte_length: bytes.len() as u64,
            media_type: if kind == SaveAttachmentKind::Note {
                Some("text/plain; charset=utf-8")
            } else {
                detect_media_type(&bytes)
            },
        })
    }

    pub(super) async fn remove_attachment(
        &self,
        depot: SaveDepotId,
        commit: SaveCommitId,
        kind: SaveAttachmentKind,
    ) -> Result<bool, Error> {
        let _guard = self.inner.operation.lock().await;
        self.ensure_member(depot, commit)?;
        let repo = self.inner.repository().to_thread_local();
        let reference = self.refs(depot).attachment(kind).to_owned();
        let Some(parent) = Self::tip_in(&repo, &reference)? else {
            return Ok(false);
        };
        let root = repo
            .find_tree(Self::commit_tree(&repo, parent)?)
            .map_err(|error| Error::git("read notes tree", error))?;
        let path = Self::note_path(commit);
        if !Self::tree_files(&repo, root.id)?.contains_key(&path) {
            return Ok(false);
        }
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit notes tree", error))?;
        editor
            .remove_leaf(path)
            .map_err(|error| Error::git("remove note", error))?;
        let tree = editor
            .write()
            .map_err(|error| Error::git("write notes tree", error))?
            .detach();
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut signature_time);
        repo.commit_as(
            signature,
            signature,
            reference,
            format!("Remove Polymer {} for {commit}", kind.as_str()),
            tree,
            [parent],
        )
        .map_err(|error| Error::git("commit note removal", error))?;
        Ok(true)
    }
}

fn detect_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if polymer_banner::ps2::Ps2Icon::parse(bytes).is_some() {
        Some(polymer_banner::ps2::Ps2Icon::MEDIA_TYPE)
    } else {
        None
    }
}
