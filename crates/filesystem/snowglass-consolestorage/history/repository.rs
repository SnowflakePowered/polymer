use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use gix::objs::Write;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    AttachmentTips, HistoryInner, MARKER_FILE, Marker, SCHEMA_VERSION, SaveCommitId, SaveDepotId,
    SaveRefState, SaveRefs, SaveVersioning,
};
use crate::Error;
use crate::savegame::SaveBlob;

impl SaveVersioning {
    pub(super) fn open(root: &Path, platform: &str, volume_id: Uuid) -> Result<Self, Error> {
        let root = root.to_owned();
        let git_dir = root.join(".git");
        let repository = if git_dir.exists() {
            gix::open(&root)
                .map_err(|error| Error::git("open repository", error))?
                .into_sync()
        } else {
            let options = gix::create::Options {
                destination_must_be_empty: Some(false),
                ..Default::default()
            };
            gix::ThreadSafeRepository::init(&root, gix::create::Kind::WithWorktree, options)
                .map_err(|error| Error::git("initialize repository", error))?
        };

        let marker_path = repository.git_dir().join(MARKER_FILE);
        Self::configure_git_repository(&repository)?;
        let expected = Marker {
            schema: SCHEMA_VERSION,
            platform: platform.to_owned(),
            volume_id,
        };
        if marker_path.exists() {
            let bytes =
                std::fs::read(&marker_path).map_err(|error| Error::io(&marker_path, error))?;
            let marker: Marker = serde_json::from_slice(&bytes)
                .map_err(|error| Error::git("read Snowglass repository marker", error))?;
            if marker.schema != expected.schema
                || marker.platform != expected.platform
                || marker.volume_id != expected.volume_id
            {
                return Err(Error::HistoryConflict(format!(
                    "{} belongs to schema {} platform {} volume {}",
                    git_dir.display(),
                    marker.schema,
                    marker.platform,
                    marker.volume_id
                )));
            }
        } else {
            let bytes = serde_json::to_vec_pretty(&expected)
                .map_err(|error| Error::git("encode repository marker", error))?;
            std::fs::write(&marker_path, bytes).map_err(|error| Error::io(&marker_path, error))?;
            let exclude = repository.git_dir().join("info").join("exclude");
            let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
            if !existing.contains("# snowglass canonical storage") {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&exclude)
                    .map_err(|error| Error::io(&exclude, error))?;
                std::io::Write::write_all(&mut file, b"\n# snowglass canonical storage\n*\n")
                    .map_err(|error| Error::io(&exclude, error))?;
            }
        }

        Ok(Self {
            inner: Arc::new(HistoryInner {
                root,
                platform: platform.to_owned(),
                volume_id,
                repository,
                operation: Mutex::new(()),
            }),
        })
    }

    fn configure_git_repository(repository: &gix::ThreadSafeRepository) -> Result<(), Error> {
        let config_path = repository.git_dir().join("config");
        let existing = std::fs::read_to_string(&config_path)
            .map_err(|error| Error::io(&config_path, error))?;
        if existing.contains("# snowglass history policy v1") {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&config_path)
            .map_err(|error| Error::io(&config_path, error))?;
        std::io::Write::write_all(
            &mut file,
            b"\n# snowglass history policy v1\n[core]\n\tlogAllRefUpdates = always\n[gc \"refs/snowglass/archive/v1/recently-deleted\"]\n\treflogExpireUnreachable = 30 days\n",
        )
        .map_err(|error| Error::io(&config_path, error))
    }

    pub(super) fn refs(&self, depot: SaveDepotId) -> SaveRefs {
        let save = format!(
            "refs/snowglass/saves/v1/{}/{:016x}",
            self.inner.platform, depot.0
        );
        let attachments = format!(
            "refs/notes/snowglass/v1/{}/{:016x}",
            self.inner.platform, depot.0
        );
        SaveRefs {
            head: save,
            screenshot: format!("{attachments}/screenshot"),
            icon: format!("{attachments}/icon"),
            note: format!("{attachments}/note"),
        }
    }

    pub(super) fn tracked_depots(&self) -> Result<Vec<SaveDepotId>, Error> {
        let repo = self.inner.repository.to_thread_local();
        let prefix = format!("refs/snowglass/saves/v1/{}/", self.inner.platform);
        let mut depots = Vec::new();
        let references = repo
            .references()
            .map_err(|error| Error::git("list save references", error))?;
        for reference in references
            .all()
            .map_err(|error| Error::git("list save references", error))?
        {
            let reference = reference.map_err(|error| Error::git("read save reference", error))?;
            let name = reference.name().as_bstr().to_str_lossy();
            let Some(value) = name.strip_prefix(&prefix) else {
                continue;
            };
            if let Ok(value) = u64::from_str_radix(value, 16) {
                depots.push(SaveDepotId(value));
            }
        }
        Ok(depots)
    }

    pub(super) fn tip_in(
        repo: &gix::Repository,
        name: &str,
    ) -> Result<Option<gix::ObjectId>, Error> {
        repo.try_find_reference(name)
            .map_err(|error| Error::git("find reference", error))
            .map(|reference| {
                reference.and_then(|reference| reference.try_id().map(|id| id.detach()))
            })
    }

    pub(super) fn tip(&self, depot: SaveDepotId) -> Result<Option<gix::ObjectId>, Error> {
        Ok(self.ref_state(depot)?.map(|state| state.tip))
    }

    pub(crate) fn tip_id(&self, depot: SaveDepotId) -> Result<Option<SaveCommitId>, Error> {
        self.tip(depot).map(|tip| tip.map(SaveCommitId))
    }

    pub(super) fn ref_state_in(
        repo: &gix::Repository,
        refs: &SaveRefs,
    ) -> Result<Option<SaveRefState>, Error> {
        let Some(tip) = Self::tip_in(repo, &refs.head)? else {
            return Ok(None);
        };
        Ok(Some(SaveRefState {
            tip,
            attachments: AttachmentTips {
                screenshot: Self::tip_in(repo, &refs.screenshot)?,
                icon: Self::tip_in(repo, &refs.icon)?,
                note: Self::tip_in(repo, &refs.note)?,
            },
        }))
    }

    pub(super) fn ref_state(&self, depot: SaveDepotId) -> Result<Option<SaveRefState>, Error> {
        Self::ref_state_in(&self.inner.repository.to_thread_local(), &self.refs(depot))
    }

    fn validate_blob_path(path: &Path) -> Result<String, Error> {
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(Error::InvalidPath(path.display().to_string()));
        }
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => {
                    let value = value
                        .to_str()
                        .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?;
                    if value.is_empty() || value == ".git" || value.contains(['/', '\\']) {
                        return Err(Error::InvalidPath(path.display().to_string()));
                    }
                    parts.push(value);
                }
                _ => return Err(Error::InvalidPath(path.display().to_string())),
            }
        }
        Ok(parts.join("/"))
    }

    pub(super) fn write_snapshot(
        &self,
        repo: &gix::Repository,
        blobs: &[SaveBlob],
    ) -> Result<gix::ObjectId, Error> {
        let mut paths = BTreeSet::new();
        let empty = repo.empty_tree();
        let mut editor = empty
            .edit()
            .map_err(|error| Error::git("start tree editor", error))?;
        for blob in blobs {
            let path = Self::validate_blob_path(&blob.target_path)?;
            if !paths.insert(path.clone()) {
                return Err(Error::HistoryConflict(format!(
                    "duplicate save path `{path}`"
                )));
            }
            let id = repo
                .write_blob(&blob.bytes)
                .map_err(|error| Error::git("write save blob", error))?
                .detach();
            editor
                .upsert(path, gix::objs::tree::EntryKind::Blob, id)
                .map_err(|error| Error::git("edit save tree", error))?;
        }
        editor
            .write()
            .map(|id| id.detach())
            .map_err(|error| Error::git("write save tree", error))
    }

    pub(super) fn signature() -> gix::actor::Signature {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let time = format!("{seconds} +0000");
        gix::actor::SignatureRef {
            name: b"Snowglass".as_bstr(),
            email: b"history@snowglass.invalid".as_bstr(),
            time: &time,
        }
        .to_owned()
        .expect("Snowglass generates a valid Git timestamp")
    }

    pub(super) fn tree_files(
        repo: &gix::Repository,
        tree: gix::ObjectId,
    ) -> Result<BTreeMap<String, Vec<u8>>, Error> {
        let tree = repo
            .find_tree(tree)
            .map_err(|error| Error::git("find save tree", error))?;
        let entries = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|error| Error::git("traverse save tree", error))?;
        let mut files = BTreeMap::new();
        for entry in entries {
            if entry.mode.kind() != gix::objs::tree::EntryKind::Blob {
                continue;
            }
            let path = entry
                .filepath
                .to_str()
                .map_err(|error| Error::git("decode save path", error))?
                .to_owned();
            let blob = repo
                .find_blob(entry.oid)
                .map_err(|error| Error::git("read save blob", error))?;
            files.insert(path, blob.data.clone());
        }
        Ok(files)
    }

    pub(super) fn commit_files(
        repo: &gix::Repository,
        commit: gix::ObjectId,
    ) -> Result<BTreeMap<String, Vec<u8>>, Error> {
        let tree = Self::commit_tree(repo, commit)?;
        Self::tree_files(repo, tree)
    }

    pub(super) fn commit_tree(
        repo: &gix::Repository,
        commit: gix::ObjectId,
    ) -> Result<gix::ObjectId, Error> {
        repo.find_commit(commit)
            .map_err(|error| Error::git("find commit", error))?
            .tree_id()
            .map(|id| id.detach())
            .map_err(|error| Error::git("read commit tree", error))
    }

    pub(super) fn install_save_refs(
        &self,
        repo: &gix::Repository,
        depot: SaveDepotId,
        state: Option<SaveRefState>,
    ) -> Result<(), Error> {
        let refs = self.refs(depot);
        let mut edits = Vec::new();
        for (name, kind) in refs.entries() {
            let value = match kind {
                Some(kind) => state.and_then(|state| state.attachments.get(kind)),
                None => state.map(|state| state.tip),
            };
            match value {
                Some(id) => edits.push(Self::ref_update(
                    name,
                    id,
                    if kind.is_some() {
                        "snowglass: install notes"
                    } else {
                        "snowglass: install save"
                    },
                )?),
                None => {
                    if let Some(reference) = repo
                        .try_find_reference(name)
                        .map_err(|error| Error::git("find save reference", error))?
                    {
                        edits.push(RefEdit {
                            change: Change::Delete {
                                expected: PreviousValue::MustExistAndMatch(
                                    reference.target().to_owned().into(),
                                ),
                                log: RefLog::AndReference,
                            },
                            name: gix::refs::FullName::try_from(name)
                                .map_err(|error| Error::git("validate reference name", error))?,
                            deref: false,
                        });
                    }
                }
            }
        }
        if edits.is_empty() {
            return Ok(());
        }
        let signature = Self::signature();
        let mut signature_time = gix::date::parse::TimeBuf::default();
        repo.edit_references_as(edits, Some(signature.to_ref(&mut signature_time)))
            .map(|_| ())
            .map_err(|error| Error::git("install save references", error))
    }

    fn ref_update(name: &str, id: gix::ObjectId, message: &str) -> Result<RefEdit, Error> {
        Ok(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: message.into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(id),
            },
            name: gix::refs::FullName::try_from(name)
                .map_err(|error| Error::git("validate reference name", error))?,
            deref: false,
        })
    }
}

pub(super) fn copy_object_graph(
    source: &gix::Repository,
    destination: &gix::Repository,
    root: gix::ObjectId,
) -> Result<(), Error> {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) || destination.has_object(id) {
            continue;
        }
        let object = source
            .find_object(id)
            .map_err(|error| Error::git("read source object", error))?;
        match object.kind {
            gix::objs::Kind::Commit => {
                let commit = object
                    .try_to_commit_ref()
                    .map_err(|error| Error::git("decode source commit", error))?;
                pending.push(commit.tree());
                pending.extend(commit.parents());
            }
            gix::objs::Kind::Tree => {
                let tree = gix::objs::TreeRef::from_bytes(&object.data, id.kind())
                    .map_err(|error| Error::git("decode source tree", error))?;
                pending.extend(tree.entries.into_iter().map(|entry| entry.oid.to_owned()));
            }
            gix::objs::Kind::Tag => {
                let tag = object
                    .try_to_tag_ref()
                    .map_err(|error| Error::git("decode source tag", error))?;
                pending.push(tag.target());
            }
            gix::objs::Kind::Blob => {}
        }
        let written = destination
            .objects
            .write_buf_with_known_id(object.kind, &object.data, id)
            .map_err(|error| Error::git("write destination object", error))?;
        if written != id {
            return Err(Error::HistoryConflict(format!(
                "object id changed while copying {id}"
            )));
        }
    }
    Ok(())
}
