use super::*;

impl FilesystemState {
    /// Standalone images and extracted packages use the same mounting contract.
    pub(super) fn mount_package_source(
        &mut self,
        mut entrypoint: VfsPathBuf,
        source: PathBuf,
        name: Option<String>,
        permissions: Permissions,
    ) -> Result<VfsPathBuf, VfsError> {
        if !source.is_absolute() {
            return Err(VfsError::Unsupported(
                "package source must be an absolute host path".into(),
            ));
        }
        let metadata = std::fs::metadata(&source)?;
        let authority = Permissions::from_str("rx").expect("canonical package permissions");
        if !authority.contains(permissions) {
            return Err(VfsError::AuthorityExceeded {
                authority,
                requested: permissions,
            });
        }
        if metadata.is_dir() && name.is_none() {
            let resource = Resource::new(
                Arc::new(PackageDirectoryFactory::new(source)),
                authority,
                ResourceOrigin::HostFile,
            );
            let handle = self.insert_volume(resource);
            self.plan
                .mount_tree(entrypoint.clone(), self.volume(handle)?, permissions);
        } else if metadata.is_file() {
            let name = name
                .as_deref()
                .or_else(|| source.file_name().and_then(|name| name.to_str()))
                .ok_or_else(|| {
                    VfsError::Unsupported("package entrypoint has no UTF-8 filename".into())
                })?;
            entrypoint = entrypoint.as_path().join(&VfsComponent::new(name)?);
            let resource = Resource::new(
                Arc::new(HostFileFactory::new(source, false)),
                authority,
                ResourceOrigin::HostFile,
            );
            let handle = self.insert_file(resource);
            self.plan
                .mount_file(entrypoint.clone(), self.file(handle)?, permissions);
        } else {
            return Err(VfsError::Unsupported(
                "package source must be a regular file or directory".into(),
            ));
        }
        self.used = true;
        Ok(entrypoint)
    }
}
