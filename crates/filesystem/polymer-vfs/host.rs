use crate::{FinalizationReason, PreparedFilesystem, VfsError, VfsPath, VfsResult};
use std::sync::Arc;

pub struct PreparedFilesystemLaunch {
    endpoint: FilesystemEndpoint,
    filesystem: Arc<PreparedFilesystem>,
}

impl PreparedFilesystemLaunch {
    pub fn new(endpoint: FilesystemEndpoint, filesystem: Arc<PreparedFilesystem>) -> Self {
        Self {
            endpoint,
            filesystem,
        }
    }

    pub fn endpoint(&self) -> &FilesystemEndpoint {
        &self.endpoint
    }
    pub fn filesystem(&self) -> &Arc<PreparedFilesystem> {
        &self.filesystem
    }

    pub fn validate_command(&self, command: &str) -> VfsResult<()> {
        let path = self
            .endpoint
            .unproject(command)
            .ok_or_else(|| VfsError::InvalidPath {
                path: command.to_owned(),
                reason: "command is outside its prepared filesystem endpoint".to_owned(),
            })?;
        let binding = self
            .filesystem
            .compiled()
            .resolve(&path)?
            .ok_or_else(|| VfsError::NotFound(path.clone()))?;
        if !binding.permissions().can_execute() {
            return Err(VfsError::PermissionDenied(path));
        }
        Ok(())
    }
}

impl std::fmt::Debug for PreparedFilesystemLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedFilesystemLaunch")
            .field("endpoint", &self.endpoint)
            .field("bindings", &self.filesystem.compiled().bindings().len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemEndpoint {
    nonce: String,
}

impl FilesystemEndpoint {
    pub fn random() -> Self {
        Self {
            nonce: format!("{:016x}", rand::random::<u64>()),
        }
    }

    pub fn project(&self, path: &VfsPath) -> String {
        #[cfg(windows)]
        {
            let suffix = path
                .to_string_lossy()
                .trim_start_matches('/')
                .replace('/', "\\");
            if suffix.is_empty() {
                format!(r"\\Polymer\{}", self.nonce)
            } else {
                format!(r"\\Polymer\{}\{}", self.nonce, suffix)
            }
        }
        #[cfg(unix)]
        {
            path.to_string_lossy()
        }
    }

    pub fn unproject(&self, projected: &str) -> Option<crate::VfsPathBuf> {
        #[cfg(windows)]
        {
            let root = format!(r"\\Polymer\{}", self.nonce);
            let suffix = projected.strip_prefix(&root)?;
            if !suffix.is_empty() && !suffix.starts_with('\\') {
                return None;
            }
            let path = format!("/{}", suffix.trim_start_matches('\\').replace('\\', "/"));
            crate::VfsPathBuf::new(path).ok()
        }
        #[cfg(unix)]
        {
            crate::VfsPathBuf::new(projected).ok()
        }
    }
}

pub trait FilesystemHost: Send + Sync {
    fn supported(&self) -> bool;
    fn allocate_endpoint(&self) -> FilesystemEndpoint;
    fn mount(
        &self,
        endpoint: FilesystemEndpoint,
        filesystem: Arc<PreparedFilesystem>,
    ) -> VfsResult<MountedFilesystem>;
}

pub struct MountedFilesystem {
    endpoint: FilesystemEndpoint,
    filesystem: Arc<PreparedFilesystem>,
    backend: Option<Box<dyn Send>>,
}

impl MountedFilesystem {
    pub fn new(
        endpoint: FilesystemEndpoint,
        filesystem: Arc<PreparedFilesystem>,
        backend: impl Send + 'static,
    ) -> Self {
        Self {
            endpoint,
            filesystem,
            backend: Some(Box::new(backend)),
        }
    }

    pub fn endpoint(&self) -> &FilesystemEndpoint {
        &self.endpoint
    }

    pub fn filesystem(&self) -> &PreparedFilesystem {
        &self.filesystem
    }

    pub fn finish(mut self, reason: FinalizationReason) -> VfsResult<()> {
        self.backend.take();
        self.filesystem.finalize(reason)
    }
}

impl Drop for MountedFilesystem {
    fn drop(&mut self) {
        self.backend.take();
        let _ = self.filesystem.finalize(FinalizationReason::Cancelled);
    }
}

#[derive(Default)]
pub struct MockFilesystemHost;

impl FilesystemHost for MockFilesystemHost {
    fn supported(&self) -> bool {
        true
    }

    fn allocate_endpoint(&self) -> FilesystemEndpoint {
        FilesystemEndpoint::random()
    }

    fn mount(
        &self,
        endpoint: FilesystemEndpoint,
        filesystem: Arc<PreparedFilesystem>,
    ) -> VfsResult<MountedFilesystem> {
        filesystem.materialize()?;
        Ok(MountedFilesystem::new(endpoint, filesystem, ()))
    }
}

#[derive(Default)]
pub struct UnsupportedFilesystemHost;

impl FilesystemHost for UnsupportedFilesystemHost {
    fn supported(&self) -> bool {
        false
    }

    fn allocate_endpoint(&self) -> FilesystemEndpoint {
        FilesystemEndpoint::random()
    }

    fn mount(
        &self,
        _endpoint: FilesystemEndpoint,
        _filesystem: Arc<PreparedFilesystem>,
    ) -> VfsResult<MountedFilesystem> {
        Err(VfsError::Unsupported(
            "no FUSE or WinFSP filesystem host is installed".to_owned(),
        ))
    }
}
