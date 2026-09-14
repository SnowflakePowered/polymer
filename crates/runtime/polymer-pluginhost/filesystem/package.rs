use super::*;

impl FilesystemState {
    /// Standalone images and extracted packages use the same mounting contract.
    pub(super) fn mount_package_source(
        &mut self,
        target: String,
        source: PathBuf,
    ) -> Result<String, VfsError> {
        if !source.is_absolute() {
            return Err(VfsError::Unsupported(
                "package source must be an absolute host path".into(),
            ));
        }
        let metadata = std::fs::metadata(&source)?;
        let mut entrypoint = VfsPathBuf::new(if target.is_empty() { "/" } else { &target })?;
        // Directory traversal requires execute permission; neither form permits writes.
        let mode = if metadata.is_dir() { "rx" } else { "r" };
        let permissions = Permissions::from_str(mode).expect("canonical package permissions");
        if metadata.is_dir() {
            let resource = Resource::new(
                Arc::new(PackageDirectoryFactory::new(source)),
                permissions,
                ResourceOrigin::HostFile,
            );
            let handle = self.insert_volume(resource);
            self.plan
                .mount_tree(entrypoint.clone(), self.volume(handle)?, permissions);
        } else if metadata.is_file() {
            let name = source
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    VfsError::Unsupported("package entrypoint has no UTF-8 filename".into())
                })?;
            entrypoint = entrypoint.as_path().join(&VfsComponent::new(name)?);
            let resource = Resource::new(
                Arc::new(HostFileFactory::new(source, false)),
                permissions,
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
        Ok(self.endpoint.project(entrypoint.as_path()))
    }
}
