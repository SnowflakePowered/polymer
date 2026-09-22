use super::*;

/// A child entry keeps the source volume's lease and authority when mounted elsewhere.
struct EntryFactory {
    source: Arc<VolumeResource>,
    components: Vec<String>,
}

impl ResourceFactory<dyn FileNode> for EntryFactory {
    fn materialize(&self) -> Result<Materialized<dyn FileNode>, VfsError> {
        let source = self.source.materialize()?;
        let (name, parents) = self.components.split_last().ok_or(VfsError::StaleNode)?;
        let directory = FilesystemState::directory_at(source.backend.root()?, parents)?;
        let backend = match directory.lookup(&VfsComponent::new(name)?)? {
            Some(Node::File(file)) => file,
            Some(_) => return Err(VfsError::IsDirectory(VfsPathBuf::root())),
            None => return Err(VfsError::NotFound(VfsPathBuf::root())),
        };
        Ok(Materialized {
            backend,
            lease: source.lease,
        })
    }
}

impl ResourceFactory<dyn Volume> for EntryFactory {
    fn materialize(&self) -> Result<Materialized<dyn Volume>, VfsError> {
        let source = self.source.materialize()?;
        let directory = FilesystemState::directory_at(source.backend.root()?, &self.components)?;
        let mut path = VfsPathBuf::root();
        for component in &self.components {
            path = path.as_path().join(&VfsComponent::new(component)?);
        }
        Ok(Materialized {
            backend: Arc::new(EntryVolume {
                source: source.backend,
                directory,
                path,
            }),
            lease: source.lease,
        })
    }
}

struct EntryVolume {
    source: Arc<dyn Volume>,
    directory: Arc<dyn DirectoryNode>,
    path: VfsPathBuf,
}

impl EntryVolume {
    fn source_path(&self, path: &polymer_vfs::VfsPath) -> Result<VfsPathBuf, VfsError> {
        let mut result = self.path.clone();
        for component in path.components() {
            result = result.as_path().join(&VfsComponent::from_units(
                polymer_vfs::PathStr::as_slice(component).to_vec(),
            )?);
        }
        Ok(result)
    }
}

impl Volume for EntryVolume {
    fn root(&self) -> Result<Arc<dyn DirectoryNode>, VfsError> {
        Ok(self.directory.clone())
    }

    fn rename(
        &self,
        source: &polymer_vfs::VfsPath,
        target: &polymer_vfs::VfsPath,
    ) -> Result<(), VfsError> {
        self.source.rename(
            self.source_path(source)?.as_path(),
            self.source_path(target)?.as_path(),
        )
    }

    fn replace(
        &self,
        source: &polymer_vfs::VfsPath,
        target: &polymer_vfs::VfsPath,
    ) -> Result<(), VfsError> {
        self.source.replace(
            self.source_path(source)?.as_path(),
            self.source_path(target)?.as_path(),
        )
    }

    fn detach(&self, source: &polymer_vfs::VfsPath) -> Result<(), VfsError> {
        self.source.detach(self.source_path(source)?.as_path())
    }
}

impl FilesystemState {
    pub(super) fn mount_entry(
        &mut self,
        target: VfsPathBuf,
        handle: ResourceHandle,
        components: Vec<String>,
        directory: bool,
        permissions: Permissions,
    ) -> Result<(), VfsError> {
        let authority = match handle {
            ResourceHandle::File(handle) => self.file(handle)?.authority(),
            ResourceHandle::Tree(handle) => self.volume(handle)?.authority(),
        };
        if !authority.contains(permissions) {
            return Err(VfsError::AuthorityExceeded {
                authority,
                requested: permissions,
            });
        }
        match handle {
            ResourceHandle::File(handle) if components.is_empty() && !directory => {
                self.plan
                    .mount_file(target, self.file(handle)?, permissions);
            }
            ResourceHandle::Tree(handle) => {
                let source = self.volume(handle)?;
                if !source.authority().can_execute() {
                    return Err(VfsError::PermissionDenied(target));
                }
                let authority = source.authority();
                let origin = source.origin().clone();
                if components.is_empty() && directory {
                    self.plan.mount_tree(target, source, permissions);
                } else if directory {
                    self.plan.mount_tree(
                        target,
                        Arc::new(Resource::new(
                            Arc::new(EntryFactory { source, components }),
                            authority,
                            origin,
                        )),
                        permissions,
                    );
                } else {
                    self.plan.mount_file(
                        target,
                        Arc::new(Resource::new(
                            Arc::new(EntryFactory { source, components }),
                            authority,
                            origin,
                        )),
                        permissions,
                    );
                }
            }
            _ => return Err(VfsError::Unsupported("invalid file entry".into())),
        }
        self.used = true;
        Ok(())
    }
}
