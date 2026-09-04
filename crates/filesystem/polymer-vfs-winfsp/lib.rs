//! WinFSP adapter for Polymer's platform-neutral virtual filesystem.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::WinFspFilesystemHost;

#[cfg(not(windows))]
mod unsupported {
    use polymer_vfs::{
        FilesystemEndpoint, FilesystemHost, MountedFilesystem, PreparedFilesystem, VfsError,
        VfsResult,
    };
    use std::sync::Arc;

    #[derive(Clone, Copy, Debug, Default)]
    pub struct WinFspFilesystemHost;

    impl WinFspFilesystemHost {
        pub const fn with_dispatcher_threads(_dispatcher_threads: u32) -> Self {
            Self
        }
    }

    impl FilesystemHost for WinFspFilesystemHost {
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
                "WinFSP is only available on Windows".into(),
            ))
        }
    }
}

#[cfg(not(windows))]
pub use unsupported::WinFspFilesystemHost;
