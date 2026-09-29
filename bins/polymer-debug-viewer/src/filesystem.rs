//! VFS preparation for the debug viewer's command-line launcher.

use polymer_vfs::{
    FilesystemHost, FilesystemPlan, HostDirectoryFactory, HostFileFactory, MountedFilesystem,
    Permissions, PreparedFilesystemLaunch, Resource, ResourceOrigin, VfsPathBuf,
};
use std::{error::Error, path::Path, sync::Arc};

pub(crate) struct DebugFilesystem;

impl DebugFilesystem {
    pub(crate) fn mount(target: &mut [String]) -> Result<MountedFilesystem, Box<dyn Error>> {
        let executable = std::fs::canonicalize(&target[0])?;
        #[cfg(target_os = "macos")]
        let staged = executable
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "app"))
            .map(|bundle| {
                polymer_platform_macos::StagedBundle::prepare(
                    bundle,
                    &polymer_platform_macos::StagedBundle::cache_root()?,
                )
                .map(Arc::new)
            })
            .transpose()?;
        #[cfg(target_os = "macos")]
        let executable = staged
            .as_ref()
            .map_or(executable.clone(), |bundle| bundle.executable().to_owned());
        let directory = executable
            .parent()
            .ok_or("guest executable has no parent")?;
        #[cfg(target_os = "macos")]
        let directory = staged.as_ref().map_or(directory, |bundle| bundle.path());
        let factory: Arc<dyn polymer_vfs::ResourceFactory<dyn polymer_vfs::Volume>> =
            Arc::new(HostDirectoryFactory::new(directory.to_path_buf(), true));
        #[cfg(target_os = "macos")]
        let factory: Arc<dyn polymer_vfs::ResourceFactory<dyn polymer_vfs::Volume>> =
            staged.as_ref().map_or(factory, |bundle| bundle.clone());
        let host = polymer_runtime::filesystem_host();
        let endpoint = host.allocate_endpoint();
        let mut plan = FilesystemPlan::default();
        let permissions = Permissions::READ | Permissions::WRITE | Permissions::EXECUTE;
        plan.mount_tree(
            VfsPathBuf::new("/emulator")?,
            Arc::new(Resource::new(
                factory,
                permissions,
                ResourceOrigin::HostFile,
            )),
            permissions,
        );
        for (index, argument) in target.iter_mut().enumerate() {
            // Preserve flags and non-path arguments verbatim. Whole path arguments
            // are projected into the same mounted namespace as the executable.
            let path = if index == 0 {
                executable.clone()
            } else if Path::new(argument).exists() {
                std::fs::canonicalize(&argument)?
            } else {
                continue;
            };
            let virtual_path = if let Ok(relative) = path.strip_prefix(directory) {
                VfsPathBuf::new(format!(
                    "/emulator/{}",
                    relative.to_string_lossy().replace('\\', "/")
                ))?
            } else {
                let at = VfsPathBuf::new(format!(
                    "/arguments/{index}/{}",
                    path.file_name()
                        .ok_or("argument has no file name")?
                        .to_string_lossy()
                ))?;
                if path.is_dir() {
                    plan.mount_tree(
                        at.clone(),
                        Arc::new(Resource::new(
                            Arc::new(HostDirectoryFactory::new(path, true)),
                            permissions,
                            ResourceOrigin::HostFile,
                        )),
                        permissions,
                    );
                } else {
                    plan.mount_file(
                        at.clone(),
                        Arc::new(Resource::new(
                            Arc::new(HostFileFactory::new(path, true)),
                            permissions,
                            ResourceOrigin::HostFile,
                        )),
                        permissions,
                    );
                }
                at
            };
            *argument = endpoint.project(virtual_path.as_path());
        }
        let prepared = PreparedFilesystemLaunch::new(endpoint, Arc::new(plan.seal()?));
        polymer_runtime::mount_prepared_filesystem(&host, prepared, &target[0])
            .map_err(|error| std::io::Error::other(error).into())
    }
}
