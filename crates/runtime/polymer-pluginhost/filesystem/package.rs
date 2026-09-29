use super::mount::{MountSource, PackageBundle};
use super::*;
use polymer_library::{LaunchableUri, ResolvedLaunchable};

impl FilesystemState {
    pub(super) fn mount_package(
        &mut self,
        target: VfsPathBuf,
        uri: LaunchableUri,
        bundle: Option<PackageBundle>,
        permissions: Permissions,
    ) -> Result<VfsPathBuf, VfsError> {
        let authority = Permissions::from_str("rx").expect("canonical package permissions");
        if !authority.contains(permissions) {
            return Err(VfsError::AuthorityExceeded {
                authority,
                requested: permissions,
            });
        }
        let mounted = match bundle {
            None => self.mount_launchable(target, &uri, authority, permissions)?,
            Some(bundle) => self.mount_bundle(target, bundle, authority, permissions)?,
        };
        self.used = true;
        Ok(mounted)
    }

    fn mount_launchable(
        &mut self,
        target: VfsPathBuf,
        uri: &LaunchableUri,
        authority: Permissions,
        permissions: Permissions,
    ) -> Result<VfsPathBuf, VfsError> {
        match uri.resolve()? {
            ResolvedLaunchable::Directory { path, entrypoint } => {
                let handle = self.insert_volume(Resource::new(
                    Arc::new(PackageDirectoryFactory::new(path)),
                    authority,
                    ResourceOrigin::HostFile,
                ));
                self.plan
                    .mount_tree(target.clone(), self.volume(handle)?, permissions);
                entrypoint
                    .iter()
                    .flat_map(|entrypoint| entrypoint.split('/'))
                    .try_fold(target, |path, component| {
                        Ok(path.as_path().join(&VfsComponent::new(component)?))
                    })
            }
            ResolvedLaunchable::File(path) => {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        VfsError::Unsupported("package entrypoint has no UTF-8 filename".into())
                    })?;
                let target = target.as_path().join(&VfsComponent::new(name)?);
                self.mount_host_file(target.clone(), path, authority, permissions)?;
                Ok(target)
            }
        }
    }

    fn mount_bundle(
        &mut self,
        target: VfsPathBuf,
        bundle: PackageBundle,
        authority: Permissions,
        permissions: Permissions,
    ) -> Result<VfsPathBuf, VfsError> {
        let mut names = std::collections::HashSet::new();
        let valid = bundle.files.iter().all(|file| {
            !file.name.contains([':', '\\'])
                && names.insert(file.name.as_str())
                && file.uri.is_some() != file.text.is_some()
        });
        if !valid || !names.contains(bundle.entrypoint.as_str()) {
            return Err(VfsError::Unsupported("invalid package bundle".into()));
        }
        let entrypoint = target
            .as_path()
            .join(&VfsComponent::new(&bundle.entrypoint)?);
        for file in bundle.files {
            let path = target.as_path().join(&VfsComponent::new(&file.name)?);
            match (file.uri, file.text) {
                (Some(uri), _) => {
                    let ResolvedLaunchable::File(host) = uri.resolve()? else {
                        return Err(VfsError::Unsupported(
                            "package bundle files must be regular files".into(),
                        ));
                    };
                    self.mount_host_file(path, host, authority, permissions)?;
                }
                (None, text) => {
                    let contents = ByteBuf::from(text.unwrap_or_default().into_bytes());
                    self.mount(path, MountSource::Bytes { contents }, permissions)?;
                }
            }
        }
        Ok(entrypoint)
    }

    fn mount_host_file(
        &mut self,
        target: VfsPathBuf,
        path: PathBuf,
        authority: Permissions,
        permissions: Permissions,
    ) -> Result<(), VfsError> {
        let handle = self.insert_file(Resource::new(
            Arc::new(HostFileFactory::new(path, false)),
            authority,
            ResourceOrigin::HostFile,
        ));
        self.plan
            .mount_file(target, self.file(handle)?, permissions);
        Ok(())
    }
}
