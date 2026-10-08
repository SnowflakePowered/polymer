use super::*;

#[derive(Clone)]
pub(in crate::storage::git) struct ArtifactRecord {
    pub(in crate::storage::git) id: String,
    pub(super) depot_type: DepotType,
    pub(super) manifest: Vec<ArtifactFileRecord>,
    pub(super) label: String,
    pub(super) version: Option<String>,
    pub(super) icon_path: Option<PathBuf>,
    pub(super) icons: Vec<EmbeddedIcon>,
    pub(super) byte_length: u64,
    pub(super) installed: Timestamp,
    pub(super) entrypoint: Option<ArtifactEntrypoint>,
}

#[derive(Clone)]
pub(super) struct ArtifactFileRecord {
    pub(super) path: PathBuf,
    pub(super) blake3: [u8; 32],
    pub(super) byte_length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TreeLeaf {
    pub(super) kind: gix::objs::tree::EntryKind,
    pub(super) id: gix::ObjectId,
}

#[derive(Clone)]
pub(in crate::storage::git) struct NandCommit {
    pub(super) id: gix::ObjectId,
    pub(super) commit: gix::objs::Commit,
    pub(super) artifact: Option<String>,
}

impl From<&Artifact> for ArtifactRecord {
    fn from(artifact: &Artifact) -> Self {
        Self {
            id: artifact.id().to_owned(),
            depot_type: artifact.depot_type(),
            manifest: artifact
                .manifest()
                .iter()
                .map(|file| ArtifactFileRecord {
                    path: file.path().to_owned(),
                    blake3: *file.blake3(),
                    byte_length: file.byte_length(),
                })
                .collect(),
            label: artifact.label().to_owned(),
            version: artifact.version().map(str::to_owned),
            icon_path: artifact.icons().iter().find_map(|icon| match icon {
                IconHint::File(path) => Some(path.clone()),
                _ => None,
            }),
            icons: artifact
                .icons()
                .iter()
                .filter_map(|icon| match icon {
                    IconHint::Embedded(icon) => Some(icon.clone()),
                    _ => None,
                })
                .collect(),
            byte_length: artifact.byte_length(),
            installed: artifact.installed(),
            entrypoint: artifact.entrypoint().cloned(),
        }
    }
}

impl ArtifactRecord {
    pub(in crate::storage::git) fn artifact(&self) -> Result<Artifact, Error> {
        Artifact::new(
            self.id.clone(),
            self.depot_type,
            self.manifest
                .iter()
                .map(|file| DepotFile::new(file.path.clone(), file.blake3, file.byte_length))
                .collect(),
            self.label.clone(),
            self.version.clone(),
            match (&self.icon_path, self.icons.as_slice()) {
                (Some(path), []) => vec![IconHint::File(path.clone())],
                (None, []) => vec![IconHint::Builtin(self.depot_type)],
                (_, icons) => icons.iter().cloned().map(IconHint::Embedded).collect(),
            },
            self.entrypoint.clone(),
        )
        .map(|artifact| artifact.with_installed(self.installed))
    }

    pub(in crate::storage::git) fn write(
        &self,
        editor: &mut gix::object::tree::Editor<'_>,
        root: &str,
    ) -> Result<(), Error> {
        let repo = editor.repo;
        PlatformRepository::write_text(editor, &format!("{root}/id"), &self.id)?;
        PlatformRepository::write_text(
            editor,
            &format!("{root}/depot-type"),
            &u8::from(self.depot_type).to_string(),
        )?;
        PlatformRepository::write_text(editor, &format!("{root}/label"), &self.label)?;
        PlatformRepository::write_text(
            editor,
            &format!("{root}/byte-length"),
            &self.byte_length.to_string(),
        )?;
        if let Some(version) = &self.version {
            PlatformRepository::write_text(editor, &format!("{root}/version"), version)?;
        }
        PlatformRepository::write_text(
            editor,
            &format!("{root}/installed"),
            &self.installed.to_string(),
        )?;
        if let Some(entrypoint) = &self.entrypoint {
            PlatformRepository::write_text(editor, &format!("{root}/root"), &entrypoint.root)?;
            PlatformRepository::write_text(
                editor,
                &format!("{root}/entrypoint"),
                &entrypoint.path,
            )?;
            PlatformRepository::write_text(
                editor,
                &format!("{root}/package"),
                &entrypoint.package,
            )?;
        }
        if let Some(path) = &self.icon_path {
            PlatformRepository::write_text(
                editor,
                &format!("{root}/icon-path"),
                &path.to_string_lossy(),
            )?;
        }
        for icon in &self.icons {
            let blob = repo
                .write_blob(icon.bytes())
                .map_err(|error| Error::git("write artifact icon", error))?
                .detach();
            editor
                .upsert(
                    format!("{root}/{}", icon.file_name()),
                    gix::objs::tree::EntryKind::Blob,
                    blob,
                )
                .map_err(|error| Error::git("update artifact icon", error))?;
        }
        for file in &self.manifest {
            let path = file.path.to_string_lossy().replace('\\', "/");
            let file_root = format!("{root}/files/{}", TreeKey::new(&path));
            PlatformRepository::write_text(editor, &format!("{file_root}/path"), &path)?;
            PlatformRepository::write_text(
                editor,
                &format!("{file_root}/byte-length"),
                &file.byte_length.to_string(),
            )?;
            let digest = repo
                .write_blob(file.blake3)
                .map_err(|error| Error::git("write artifact digest", error))?
                .detach();
            editor
                .upsert(
                    format!("{file_root}/blake3"),
                    gix::objs::tree::EntryKind::Blob,
                    digest,
                )
                .map_err(|error| Error::git("update artifact digest", error))?;
        }
        Ok(())
    }
}
