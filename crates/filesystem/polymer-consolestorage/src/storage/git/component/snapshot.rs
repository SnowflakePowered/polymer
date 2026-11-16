use super::revision::RevisionMetadata;
use super::*;
use crate::nand::composition::ComponentRevision;
use std::io::Read;

struct ComponentReader<R> {
    inner: R,
    hash: blake3::Hasher,
}

impl<R: Read> Read for ComponentReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.hash.update(&buffer[..count]);
        Ok(count)
    }
}

impl ComponentHistory<'_> {
    fn snapshot(
        &self,
        revision: &ComponentRevision,
        source: &dyn Storage,
        context: &InstallContext,
    ) -> Result<gix::ObjectId, Error> {
        let component = &revision.component;
        let artifact = &revision.artifact;
        let mut editor = self
            .repo
            .empty_tree()
            .edit()
            .map_err(|error| Error::git("create component snapshot", error))?;
        context.phase(
            crate::installable::InstallPhase::Finalizing,
            artifact.byte_length(),
        );
        let mut paths = BTreeSet::new();
        for file in artifact.manifest() {
            let path = file.path().to_string_lossy().replace('\\', "/");
            if !paths.insert(path.clone()) || source.metadata(&path)?.len != file.byte_length() {
                return Err(Error::HistoryConflict(format!(
                    "invalid component file {path}"
                )));
            }
            let mut reader = ComponentReader {
                inner: Reporting {
                    inner: source.open_read(&path)?,
                    context: context.clone(),
                },
                hash: blake3::Hasher::new(),
            };
            let id = self
                .repo
                .objects
                .write_stream(gix::objs::Kind::Blob, file.byte_length(), &mut reader)
                .map_err(|error| Error::git("write component file", error))?;
            if reader.hash.finalize().as_bytes() != file.blake3() {
                return Err(Error::HistoryConflict(format!(
                    "component file changed: {path}"
                )));
            }
            editor
                .upsert(
                    format!("files/{path}"),
                    gix::objs::tree::EntryKind::Blob,
                    id,
                )
                .map_err(|error| Error::git("write component tree", error))?;
        }
        for path in &revision.directories {
            PlatformRepository::write_text(
                &mut editor,
                &format!("meta/fs/directories/{}", TreeKey::new(path)),
                path,
            )?;
        }
        ArtifactRecord::from(artifact).write(
            &mut editor,
            &format!("meta/artifacts/{}", TreeKey::new(artifact.id())),
        )?;
        let identity = serde_json::to_string(component)
            .map_err(|error| Error::git("encode component identity", error))?;
        PlatformRepository::write_text(&mut editor, "meta/component", &identity)?;
        let metadata = serde_json::to_string(&RevisionMetadata {
            sequence: revision.sequence,
            metadata: revision.metadata.clone(),
        })
        .map_err(|error| Error::git("encode component revision", error))?;
        PlatformRepository::write_text(&mut editor, "meta/revision", &metadata)?;
        editor
            .write()
            .map(|id| id.detach())
            .map_err(|error| Error::git("write component snapshot", error))
    }

    pub(in crate::storage::git) fn install(
        &self,
        component: &ArtifactComponent,
        metadata: &serde_json::Value,
        artifact: &Artifact,
        source: &dyn Storage,
        context: &InstallContext,
    ) -> Result<(), Error> {
        let old_tip = self.clean_tip()?;
        let before = self.states()?;
        let sequence = self
            .sequence(old_tip)?
            .checked_add(1)
            .ok_or_else(|| Error::HistoryConflict("component sequence exhausted".into()))?;
        let directories = std::iter::once(component.root.clone())
            .chain(
                source
                    .read_dir(&component.root, true)?
                    .filter(|entry| entry.metadata.kind == EntryKind::Directory)
                    .map(|entry| entry.path),
            )
            .collect();
        let mut revision = ComponentRevision {
            component: component.clone(),
            artifact: artifact.clone(),
            revision: String::new(),
            sequence,
            metadata: metadata.clone(),
            directories,
        };
        self.validate(&revision)?;
        self.composer()?
            .check_install(&revision, &self.contributions(&before)?)?;
        let previous = before
            .iter()
            .find(|state| state.revision.component.id == component.id);
        if previous.is_some_and(|state| state.revision.component != *component) {
            return Err(Error::HistoryConflict("component identity changed".into()));
        }
        let tree = self.snapshot(&revision, source, context)?;
        let signature = PlatformRepository::signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let tip = self
            .repo
            .new_commit_as(
                signature,
                signature,
                format!("Polymer install {}", artifact.id()),
                tree,
                previous.map(|state| state.tip),
            )
            .map_err(|error| Error::git("write component revision", error))?
            .id;
        revision.revision = tip.to_string();
        let state = ComponentState {
            reference: format!("{}{}", self.prefix(), TreeKey::new(&component.id)),
            tip,
            revision,
        };
        let mut after = before
            .iter()
            .filter(|state| state.revision.component.id != component.id)
            .cloned()
            .collect::<Vec<_>>();
        after.push(state);
        self.transition(old_tip, &before, &after)
    }
}
