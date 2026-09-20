use crate::server::Server;
use block2::{ManualBlockEncoding, RcBlock};
use objc2::{AnyThread, msg_send};
use objc2_foundation::{
    NSArray, NSError, NSFileManager, NSFileManagerUnmountOptions, NSString, NSURL,
};
use objc2_fs_kit::{FSClient, FSGenericURLResource, FSModuleIdentity};
use polymer_vfs::{
    FilesystemEndpoint, FilesystemHost, FinalizationReason, MountedFilesystem, PreparedFilesystem,
    VfsError, VfsPathBuf, VfsResult,
};
use std::{
    io,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

/// Requires the signed Polymer FSKit extension to be installed and enabled.
/// The containing application needs com.apple.developer.fskit.mount.
pub struct FsKitFilesystemHost {
    bundle_identifier: String,
}
impl FsKitFilesystemHost {
    pub fn new(bundle_identifier: impl Into<String>) -> Self {
        Self {
            bundle_identifier: bundle_identifier.into(),
        }
    }
    fn mount_volume(&self, endpoint: &FilesystemEndpoint, server: &Server) -> io::Result<Mount> {
        let root = PathBuf::from(endpoint.project(VfsPathBuf::root().as_path()));
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("invalid FSKit mount name"))?;
        let token: String = server
            .token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let url = NSURL::URLWithString(&NSString::from_str(&format!(
            "polymer-vfs://xpc/{name}/{token}"
        )))
        .ok_or_else(|| io::Error::other("invalid FSKit resource URL"))?;
        let resource =
            unsafe { FSGenericURLResource::initWithURL(FSGenericURLResource::alloc(), &url) };
        let identifier = NSString::from_str(&self.bundle_identifier);
        let options = NSArray::from_slice(&[
            &*NSString::from_str("-o"),
            &*NSString::from_str("nobrowse,nosuid,nodev"),
        ]);
        let (sender, receiver) = mpsc::channel();
        let callback = RcBlock::with_encoding::<_, _, _, MountReply>(
            move |path: *mut NSURL, error: *mut NSError| {
                let result = unsafe {
                    if let Some(error) = error.as_ref() {
                        Err(io::Error::other(error.to_string()))
                    } else if let Some(path) = path.as_ref().and_then(|url| url.path()) {
                        Ok(Mount {
                            path: PathBuf::from(path.to_string()),
                        })
                    } else {
                        Err(io::Error::other("FSKit returned no mount path"))
                    }
                };
                // If the caller timed out, dropping the unsent Mount unmounts a late
                // successful reply; it must not leave a dead VFS attached to the OS.
                let _ = sender.send(result);
            },
        );
        // objc2-fs-kit 0.3 predates this public macOS 27 API. Signature comes from
        // FSClient.h; keep native objects and the reply block in objc2.
        unsafe {
            let client = FSClient::sharedInstance();
            let _: () = msg_send![&client, mountSingleVolumeForResource: &*resource,
                bundleID: &*identifier, options: &*options, completionHandler: &*callback];
        }
        let mount = receiver
            .recv_timeout(Duration::from_secs(60))
            .map_err(io::Error::other)??;
        if mount.path != root {
            return Err(io::Error::other(format!(
                "FSKit mounted at {}, expected {}",
                mount.path.display(),
                root.display()
            )));
        }
        Ok(mount)
    }
}
impl FilesystemHost for FsKitFilesystemHost {
    fn supported(&self) -> bool {
        let (sender, receiver) = mpsc::channel();
        let identifier = self.bundle_identifier.clone();
        let callback = RcBlock::new(
            move |modules: *mut NSArray<FSModuleIdentity>, error: *mut NSError| {
                let found = unsafe {
                    error.is_null()
                        && modules.as_ref().is_some_and(|modules| {
                            modules.iter().any(|module| {
                                module.isEnabled()
                                    && module.bundleIdentifier().to_string() == identifier
                            })
                        })
                };
                let _ = sender.send(found);
            },
        );
        unsafe {
            FSClient::sharedInstance().fetchInstalledExtensionsWithCompletionHandler(&callback)
        };
        receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or(false)
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
        let result = (|| {
            let server = Server::start(filesystem.clone())?;
            let mount = self.mount_volume(&endpoint, &server)?;
            Ok::<_, io::Error>(MountedFilesystem::new(
                endpoint,
                filesystem.clone(),
                MountSession {
                    _mount: mount,
                    _server: server,
                },
            ))
        })();
        if result.is_err() {
            let _ = filesystem.finalize(FinalizationReason::LaunchFailed);
        }
        result.map_err(VfsError::from)
    }
}
struct MountReply;
// SAFETY: macOS uses 64-bit pointers. This describes a void block with two
// object arguments, including the class metadata NSXPCConnection requires.
unsafe impl ManualBlockEncoding for MountReply {
    type Arguments = (*mut NSURL, *mut NSError);
    type Return = ();
    const ENCODING_CSTR: &'static std::ffi::CStr = cr#"v24@?0@"NSURL"8@"NSError"16"#;
}
// Field order matters: unmount and drain kernel callbacks before stopping the VFS.
struct MountSession {
    _mount: Mount,
    _server: Server,
}
struct Mount {
    path: PathBuf,
}
impl Drop for Mount {
    fn drop(&mut self) {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&self.path.to_string_lossy()));
        let (sender, receiver) = mpsc::channel();
        let callback = RcBlock::new(move |error: *mut NSError| {
            let result = unsafe { error.as_ref() }
                .map_or(Ok(()), |error| Err(io::Error::other(error.to_string())));
            let _ = sender.send(result);
        });
        // FSKit mounts are owned by the system service. FileManager delegates
        // unmount authorization to Disk Arbitration; unmount(2) returns EPERM.
        unsafe {
            NSFileManager::defaultManager().unmountVolumeAtURL_options_completionHandler(
                &url,
                NSFileManagerUnmountOptions::WithoutUI,
                &callback,
            );
        }
        let result = receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(io::Error::other)
            .and_then(|result| result);
        if let Err(error) = result {
            eprintln!(
                "could not unmount Polymer FSKit volume {}: {error}",
                self.path.display()
            );
        }
    }
}
