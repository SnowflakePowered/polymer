use super::*;

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum MountSource {
    Scratch,
    Entry {
        kind: String,
        token: String,
        components: Vec<String>,
        directory: bool,
    },
    Storage {
        kind: String,
        token: String,
        format: Option<String>,
    },
    Bytes {
        contents: ByteBuf,
    },
    Package {
        uri: polymer_library::LaunchableUri,
        bundle: Option<PackageBundle>,
    },
}

#[derive(serde::Deserialize)]
pub(super) struct PackageBundle {
    pub(super) entrypoint: String,
    pub(super) files: Vec<PackageBundleFile>,
}

#[derive(serde::Deserialize)]
pub(super) struct PackageBundleFile {
    pub(super) name: String,
    pub(super) uri: Option<polymer_library::LaunchableUri>,
    pub(super) text: Option<String>,
}

impl FilesystemState {
    pub(super) fn mount(
        &mut self,
        target: VfsPathBuf,
        source: MountSource,
        permissions: Permissions,
    ) -> Result<VfsPathBuf, VfsError> {
        if self.sealed && !(self.prepared.is_some() && matches!(source, MountSource::Bytes { .. })) {
            return Err(VfsError::Unsupported(
                "the launch filesystem is sealed; only mounted file contents can be replaced".into(),
            ));
        }
        match source {
            MountSource::Scratch => {
                let ResourceHandle::Tree(handle) = self.scratch_resource()? else {
                    unreachable!("scratch resources are always trees")
                };
                self.plan
                    .mount_tree(target.clone(), self.volume(handle)?, permissions);
            }
            MountSource::Entry {
                kind,
                token,
                components,
                directory,
            } => {
                let handle = self.handle(&kind, &token)?;
                self.mount_entry(target.clone(), handle, components, directory, permissions)?;
            }
            MountSource::Storage {
                kind,
                token,
                format,
            } => {
                let volume = self.storage_volume(&kind, &token)?;
                self.plan
                    .mount_storage(target.clone(), volume, permissions, format);
            }
            MountSource::Bytes { contents } => {
                let contents = contents.into_vec();
                if self.prepared.is_some() {
                    self.staged_files.push((target.clone(), contents));
                    return Ok(target);
                }
                let factory = if permissions.can_write() {
                    MemoryFileFactory::writable(contents)
                } else {
                    MemoryFileFactory::immutable(contents)
                };
                let handle = self.insert_file(Resource::new(
                    Arc::new(factory),
                    permissions,
                    ResourceOrigin::Memory,
                ));
                self.plan
                    .mount_file(target.clone(), self.file(handle)?, permissions);
            }
            MountSource::Package { uri, bundle } => {
                return self.mount_package(target, uri, bundle, permissions);
            }
        }
        self.used = true;
        Ok(target)
    }
}
