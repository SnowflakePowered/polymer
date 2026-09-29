use super::*;

#[derive(Clone)]
pub(super) struct ArtifactRecord {
    pub(super) id: String,
    depot_type: DepotType,
    pub(super) manifest: Vec<ArtifactFileRecord>,
    label: String,
    version: Option<String>,
    icon_path: Option<PathBuf>,
    icons: Vec<EmbeddedIcon>,
    byte_length: u64,
    installed: Timestamp,
    entrypoint: Option<ArtifactEntrypoint>,
}

#[derive(Clone)]
pub(super) struct ArtifactFileRecord {
    pub(super) path: PathBuf,
    pub(super) blake3: [u8; 32],
    byte_length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TreeLeaf {
    pub(super) kind: gix::objs::tree::EntryKind,
    pub(super) id: gix::ObjectId,
}

#[derive(Clone)]
pub(super) struct NandCommit {
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
    pub(super) fn artifact(&self) -> Result<Artifact, Error> {
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

    pub(super) fn write(
        &self,
        repo: &gix::Repository,
        editor: &mut gix::object::tree::Editor<'_>,
        root: &str,
    ) -> Result<(), Error> {
        write_text(repo, editor, &format!("{root}/id"), &self.id)?;
        write_text(
            repo,
            editor,
            &format!("{root}/depot-type"),
            &u8::from(self.depot_type).to_string(),
        )?;
        write_text(repo, editor, &format!("{root}/label"), &self.label)?;
        write_text(
            repo,
            editor,
            &format!("{root}/byte-length"),
            &self.byte_length.to_string(),
        )?;
        if let Some(version) = &self.version {
            write_text(repo, editor, &format!("{root}/version"), version)?;
        }
        write_text(
            repo,
            editor,
            &format!("{root}/installed"),
            &self.installed.to_string(),
        )?;
        if let Some(entrypoint) = &self.entrypoint {
            write_text(repo, editor, &format!("{root}/root"), &entrypoint.root)?;
            write_text(
                repo,
                editor,
                &format!("{root}/entrypoint"),
                &entrypoint.path,
            )?;
        }
        if let Some(path) = &self.icon_path {
            write_text(
                repo,
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
            let file_root = format!("{root}/files/{}", directory_key(&path));
            write_text(repo, editor, &format!("{file_root}/path"), &path)?;
            write_text(
                repo,
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

/// The artifact's embedded icons, in the order of their supported formats.
pub(super) fn read_icons(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
) -> Result<Vec<EmbeddedIcon>, Error> {
    let mut icons = Vec::new();
    for name in EmbeddedIcon::FILES {
        if let Some(bytes) = artifact_value(repo, tree, name)? {
            icons.extend(EmbeddedIcon::from_file(name, bytes));
        }
    }
    Ok(icons)
}

pub(super) fn artifact_value(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    path: &str,
) -> Result<Option<Vec<u8>>, Error> {
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .map_err(|error| Error::git("lookup artifact value", error))?
    else {
        return Ok(None);
    };
    repo.find_blob(entry.oid())
        .map(|blob| Some(blob.data.clone()))
        .map_err(|error| Error::git("read artifact value", error))
}

pub(super) fn read_artifact_summary(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
) -> Result<ArtifactSummary, Error> {
    let text = |path: &str| -> Result<String, Error> {
        String::from_utf8(
            artifact_value(repo, tree, path)?
                .ok_or_else(|| Error::HistoryConflict(format!("artifact has no `{path}`")))?,
        )
        .map_err(|error| Error::git("decode artifact value", error))
    };
    let depot_type = text("depot-type")?
        .parse::<u8>()
        .map_err(|error| Error::git("parse artifact type", error))?;
    Ok(ArtifactSummary {
        id: text("id")?,
        depot_type: DepotType::try_from(depot_type)
            .map_err(|value| Error::HistoryConflict(format!("invalid artifact type {value}")))?,
        label: text("label")?,
        version: artifact_value(repo, tree, "version")?
            .map(String::from_utf8)
            .transpose()
            .map_err(|error| Error::git("decode artifact version", error))?,
        byte_length: text("byte-length")?
            .parse::<u64>()
            .map_err(|error| Error::git("parse artifact size", error))?,
        installed: text("installed")?
            .parse::<Timestamp>()
            .map_err(|error| Error::git("parse artifact install time", error))?,
        file_count: match tree
            .lookup_entry_by_path("files")
            .map_err(|error| Error::git("lookup artifact files", error))?
        {
            Some(files) => repo
                .find_tree(files.oid())
                .map_err(|error| Error::git("read artifact files", error))?
                .iter()
                .count(),
            None => 0,
        },
        icons: read_icons(repo, tree)?
            .iter()
            .map(EmbeddedIcon::data_url)
            .collect(),
        entrypoint: artifact_value(repo, tree, "root")?
            .zip(artifact_value(repo, tree, "entrypoint")?)
            .map(|(root, path)| {
                Ok::<_, std::string::FromUtf8Error>(ArtifactEntrypoint {
                    root: String::from_utf8(root)?,
                    path: String::from_utf8(path)?,
                })
            })
            .transpose()
            .map_err(|error| Error::git("decode artifact entrypoint", error))?,
    })
}

pub(super) fn read_artifact(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
) -> Result<ArtifactRecord, Error> {
    let value = |path: &str| artifact_value(repo, tree, path);
    let ArtifactSummary {
        id,
        depot_type,
        label,
        version,
        byte_length,
        installed,
        entrypoint,
        ..
    } = read_artifact_summary(repo, tree)?;
    let icon_path = value("icon-path")?
        .map(String::from_utf8)
        .transpose()
        .map_err(|error| Error::git("decode artifact icon path", error))?
        .map(PathBuf::from);
    let mut manifest = Vec::new();
    if let Some(files) = tree
        .lookup_entry_by_path("files")
        .map_err(|error| Error::git("lookup artifact files", error))?
    {
        let files = repo
            .find_tree(files.oid())
            .map_err(|error| Error::git("read artifact files", error))?;
        for file in files.iter() {
            let file = file.map_err(|error| Error::git("decode artifact file", error))?;
            let file = repo
                .find_tree(file.oid())
                .map_err(|error| Error::git("read artifact file", error))?;
            let file_value = |name: &str| -> Result<Vec<u8>, Error> {
                let entry = file
                    .lookup_entry_by_path(name)
                    .map_err(|error| Error::git("lookup artifact file value", error))?
                    .ok_or_else(|| {
                        Error::HistoryConflict(format!("artifact file has no `{name}`"))
                    })?;
                repo.find_blob(entry.oid())
                    .map(|blob| blob.data.clone())
                    .map_err(|error| Error::git("read artifact file value", error))
            };
            let path = PathBuf::from(
                String::from_utf8(file_value("path")?)
                    .map_err(|error| Error::git("decode artifact file path", error))?,
            );
            let blake3: [u8; 32] = file_value("blake3")?.try_into().map_err(|_| {
                Error::HistoryConflict("artifact digest has the wrong length".to_owned())
            })?;
            let byte_length = String::from_utf8(file_value("byte-length")?)
                .map_err(|error| Error::git("decode artifact file size", error))?
                .parse::<u64>()
                .map_err(|error| Error::git("parse artifact file size", error))?;
            manifest.push(ArtifactFileRecord {
                path,
                blake3,
                byte_length,
            });
        }
    }
    Ok(ArtifactRecord {
        id,
        depot_type,
        manifest,
        label,
        version,
        icon_path,
        icons: read_icons(repo, tree)?,
        byte_length,
        installed,
        entrypoint,
    })
}
