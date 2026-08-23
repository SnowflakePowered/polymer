use parking_lot::RwLock;
use snowglass_vfs::{
    CreateOptions, FileKind, FilesystemEndpoint, FilesystemHost, FinalizationReason, Metadata,
    MetadataChanges, MountedFilesystem, OpenFile, OpenOptions, PreparedFilesystem, RemoveKind,
    VfsError, VfsPathBuf, VfsResult,
};
use std::{
    ffi::c_void,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{
            HLOCAL, LocalFree, STATUS_ACCESS_DENIED, STATUS_DIRECTORY_NOT_EMPTY,
            STATUS_END_OF_FILE, STATUS_FILE_CORRUPT_ERROR, STATUS_FILE_INVALID,
            STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_DEVICE_REQUEST, STATUS_NOT_A_DIRECTORY,
            STATUS_NOT_SAME_DEVICE, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_INVALID,
            STATUS_OBJECT_NAME_NOT_FOUND,
        },
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            GetSecurityDescriptorLength, PSECURITY_DESCRIPTOR,
        },
        Storage::FileSystem::{
            FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
            INVALID_FILE_ATTRIBUTES,
        },
    },
    core::PCWSTR,
};
use winfsp::{
    FspError, U16CStr,
    constants::FspCleanupFlags,
    filesystem::{
        DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo,
        VolumeInfo, WideNameInfo,
    },
    host::{FileSystemHost as NativeFileSystemHost, VolumeParams},
};

const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
const FILE_READ_DATA: u32 = 0x0000_0001;
const FILE_WRITE_DATA: u32 = 0x0000_0002;
const FILE_APPEND_DATA: u32 = 0x0000_0004;
const FILE_EXECUTE: u32 = 0x0000_0020;
const ALLOCATION_UNIT: u64 = 4096;
const VOLUME_SIZE: u64 = 1 << 40;

#[cfg(test)]
static TEST_READ_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static TEST_READ_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub struct WinFspFilesystemHost {
    dispatcher_threads: u32,
}

impl WinFspFilesystemHost {
    pub const fn with_dispatcher_threads(dispatcher_threads: u32) -> Self {
        Self { dispatcher_threads }
    }
}

impl FilesystemHost for WinFspFilesystemHost {
    fn supported(&self) -> bool {
        winfsp::winfsp_init().is_ok()
    }

    fn allocate_endpoint(&self) -> FilesystemEndpoint {
        FilesystemEndpoint::random()
    }

    fn mount(
        &self,
        endpoint: FilesystemEndpoint,
        filesystem: Arc<PreparedFilesystem>,
    ) -> VfsResult<MountedFilesystem> {
        winfsp::winfsp_init().map_err(|error| host_error("load runtime", error))?;
        filesystem.materialize()?;

        let mounted = (|| {
            let context = SnowglassFileSystem::new(filesystem.clone())
                .map_err(|error| host_error("build security descriptor", error))?;
            let root = endpoint.project(VfsPathBuf::root().as_path());
            let prefix = root.strip_prefix('\\').ok_or_else(|| {
                VfsError::Unsupported(format!("invalid WinFSP endpoint '{root}'"))
            })?;
            let mut parameters = VolumeParams::new();
            parameters
                .prefix(prefix)
                .filesystem_name("Snowglass")
                .sector_size(ALLOCATION_UNIT as u16)
                .sectors_per_allocation_unit(1)
                .max_component_length(255)
                .volume_creation_time(context.created_at)
                .volume_serial_number(endpoint_serial(&root))
                .file_info_timeout(u32::MAX)
                .case_sensitive_search(false)
                .case_preserved_names(true)
                .unicode_on_disk(true)
                .persistent_acls(false)
                .read_only_volume(false)
                .allow_open_in_kernel_mode(true)
                .post_cleanup_when_modified_only(true)
                .post_disposition_only_when_necessary(true)
                .supports_posix_unlink_rename(true);
            let mut host: NativeFileSystemHost<SnowglassFileSystem> =
                NativeFileSystemHost::new(parameters, context)
                    .map_err(|error| host_error("create filesystem", error))?;
            host.start_with_threads(self.dispatcher_threads)
                .map_err(|error| host_error("start dispatcher", error))?;
            Ok::<_, VfsError>(host)
        })();

        match mounted {
            Ok(host) => Ok(MountedFilesystem::new(endpoint, filesystem, host)),
            Err(error) => {
                let _ = filesystem.finalize(FinalizationReason::LaunchFailed);
                Err(error)
            }
        }
    }
}

struct SnowglassFileSystem {
    filesystem: Arc<PreparedFilesystem>,
    security: SecurityDescriptor,
    created_at: u64,
}

struct SecurityDescriptor(Vec<u8>);

struct SnowglassFileContext {
    path: RwLock<VfsPathBuf>,
    kind: FileKind,
    metadata: RwLock<Metadata>,
    file: Option<Box<dyn OpenFile>>,
    directory_buffer: OnceLock<DirBuffer>,
    can_write: bool,
    delete_pending: AtomicBool,
}

struct ListedEntry {
    name: Vec<u16>,
    path: VfsPathBuf,
    metadata: Metadata,
}

impl SnowglassFileSystem {
    fn new(filesystem: Arc<PreparedFilesystem>) -> winfsp::Result<Self> {
        Ok(Self {
            filesystem,
            security: SecurityDescriptor::full_access()?,
            created_at: system_time_filetime(),
        })
    }

    fn open_path(
        &self,
        path: VfsPathBuf,
        create_options: u32,
        granted_access: u32,
    ) -> winfsp::Result<SnowglassFileContext> {
        let can_read = granted_access & (FILE_READ_DATA | FILE_EXECUTE) != 0;
        let can_write = granted_access & (FILE_WRITE_DATA | FILE_APPEND_DATA) != 0;
        let (metadata, file) = self
            .filesystem
            .open_with_metadata(
                &path,
                OpenOptions {
                    read: can_read,
                    write: can_write,
                    truncate: false,
                },
            )
            .map_err(map_vfs_error)?;
        if create_options & FILE_DIRECTORY_FILE != 0 && metadata.kind != FileKind::Directory {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        if create_options & FILE_NON_DIRECTORY_FILE != 0 && metadata.kind == FileKind::Directory {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        Ok(SnowglassFileContext {
            path: RwLock::new(path),
            kind: metadata.kind,
            metadata: RwLock::new(metadata),
            file,
            directory_buffer: OnceLock::new(),
            can_write,
            delete_pending: AtomicBool::new(false),
        })
    }

    fn context_metadata(&self, context: &SnowglassFileContext) -> winfsp::Result<Metadata> {
        match &context.file {
            Some(file) => file.metadata().map_err(map_vfs_error),
            None => Ok(context.metadata.read().clone()),
        }
    }

    fn context_file_info(&self, context: &SnowglassFileContext) -> winfsp::Result<FileInfo> {
        let metadata = self.context_metadata(context)?;
        Ok(to_file_info(
            &metadata,
            &context.path.read(),
            self.created_at,
        ))
    }

    fn directory_entries(&self, path: &VfsPathBuf) -> winfsp::Result<Vec<ListedEntry>> {
        let current = self.filesystem.metadata(path).map_err(map_vfs_error)?;
        let parent_path = path.parent().unwrap_or_else(|| path.clone());
        let parent = self
            .filesystem
            .metadata(&parent_path)
            .map_err(map_vfs_error)?;
        let mut entries = vec![
            ListedEntry {
                name: vec![b'.' as u16],
                path: path.clone(),
                metadata: current,
            },
            ListedEntry {
                name: vec![b'.' as u16, b'.' as u16],
                path: parent_path,
                metadata: parent,
            },
        ];
        for entry in self.filesystem.read_dir(path).map_err(map_vfs_error)? {
            let child_path = path.as_path().join(&entry.name);
            entries.push(ListedEntry {
                name: entry.name.as_slice().to_vec(),
                path: child_path,
                metadata: entry.metadata,
            });
        }
        Ok(entries)
    }
}

impl SecurityDescriptor {
    fn full_access() -> winfsp::Result<Self> {
        let sddl: Vec<u16> = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let mut size = 0;
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                Some(&mut size),
            )?;
        }
        let length = unsafe { GetSecurityDescriptorLength(descriptor) } as usize;
        let bytes =
            unsafe { std::slice::from_raw_parts(descriptor.0.cast::<u8>(), length) }.to_vec();
        unsafe {
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        }
        Ok(Self(bytes))
    }

    fn copy_to(&self, output: Option<&mut [c_void]>) -> u64 {
        if let Some(output) = output
            && output.len() >= self.0.len()
        {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.0.as_ptr(),
                    output.as_mut_ptr().cast::<u8>(),
                    self.0.len(),
                );
            }
        }
        self.0.len() as u64
    }
}

impl FileSystemContext for SnowglassFileSystem {
    type FileContext = SnowglassFileContext;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let path = vfs_path(file_name)?;
        let metadata = self.filesystem.metadata(&path).map_err(map_vfs_error)?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: self.security.copy_to(security_descriptor),
            attributes: file_attributes(&metadata),
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let path = vfs_path(file_name)?;
        let context = self.open_path(path.clone(), create_options, granted_access)?;
        *file_info.as_mut() = self.context_file_info(&context)?;
        file_info.set_normalized_name(file_name.as_slice(), Some(b'\\' as u16));
        Ok(context)
    }

    fn close(&self, _context: Self::FileContext) {}

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: u32,
        file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        if extra_buffer.is_some_and(|buffer| !buffer.is_empty()) {
            return Err(STATUS_INVALID_DEVICE_REQUEST.into());
        }
        let path = vfs_path(file_name)?;
        let kind = if create_options & FILE_DIRECTORY_FILE != 0 {
            self.filesystem.create_dir(&path).map_err(map_vfs_error)?;
            RemoveKind::Directory
        } else {
            self.filesystem
                .create_file(&path, CreateOptions { exclusive: true })
                .map_err(map_vfs_error)?;
            RemoveKind::File
        };
        let opened = (|| {
            // Open first: setting READONLY before opening would incorrectly reject the
            // write access granted as part of the same create request.
            let context = self.open_path(path.clone(), create_options, granted_access)?;
            if file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
                self.filesystem
                    .set_metadata(
                        &path,
                        MetadataChanges {
                            len: None,
                            readonly: Some(true),
                        },
                    )
                    .map_err(map_vfs_error)?;
                context.metadata.write().readonly = true;
            }
            *file_info.as_mut() = self.context_file_info(&context)?;
            file_info.set_normalized_name(file_name.as_slice(), Some(b'\\' as u16));
            Ok(context)
        })();
        if opened.is_err() {
            let _ = self.filesystem.remove(&path, kind);
        }
        opened
    }

    fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, flags: u32) {
        if let Some(file) = &context.file {
            let _ = file.flush();
        }
        let delete = context.delete_pending.swap(false, AtomicOrdering::AcqRel)
            || FspCleanupFlags::FspCleanupDelete.is_flagged(flags);
        if delete {
            let path = context.path.read().clone();
            let kind = match context.kind {
                FileKind::File => RemoveKind::File,
                FileKind::Directory => RemoveKind::Directory,
            };
            let _ = self.filesystem.remove(&path, kind);
        }
    }

    fn flush(
        &self,
        context: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let Some(context) = context else {
            return Ok(());
        };
        if let Some(file) = &context.file {
            file.sync().map_err(map_vfs_error)?;
        }
        *file_info = self.context_file_info(context)?;
        Ok(())
    }

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        *file_info = self.context_file_info(context)?;
        Ok(())
    }

    fn get_security(
        &self,
        _context: &Self::FileContext,
        security_descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        Ok(self.security.copy_to(security_descriptor))
    }

    fn overwrite(
        &self,
        context: &Self::FileContext,
        file_attributes: u32,
        _replace_file_attributes: bool,
        _allocation_size: u64,
        extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if extra_buffer.is_some_and(|buffer| !buffer.is_empty()) {
            return Err(STATUS_INVALID_DEVICE_REQUEST.into());
        }
        if !context.can_write {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        context
            .file
            .as_ref()
            .ok_or_else(|| FspError::from(STATUS_FILE_IS_A_DIRECTORY))?
            .truncate(0)
            .map_err(map_vfs_error)?;
        self.filesystem
            .set_metadata(
                &context.path.read(),
                MetadataChanges {
                    len: None,
                    readonly: Some(file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0),
                },
            )
            .map_err(map_vfs_error)?;
        context.metadata.write().readonly = file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0;
        *file_info = self.context_file_info(context)?;
        Ok(())
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if context.kind != FileKind::Directory {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        let directory_buffer = context.directory_buffer.get_or_init(DirBuffer::new);
        match directory_buffer.acquire(marker.is_none(), None) {
            Ok(directory_buffer) => {
                let path = context.path.read();
                for entry in self.directory_entries(&path)? {
                    let mut info = DirInfo::<255>::new();
                    *info.file_info_mut() =
                        to_file_info(&entry.metadata, &entry.path, self.created_at);
                    info.set_name_raw(entry.name.as_slice())?;
                    directory_buffer.write(&mut info)?;
                }
            }
            Err(error) if error.to_ntstatus() == 0 => {}
            Err(error) => return Err(error),
        }
        Ok(directory_buffer.read(marker, buffer))
    }

    fn rename(
        &self,
        context: &Self::FileContext,
        file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> winfsp::Result<()> {
        let source = vfs_path(file_name)?;
        let target = vfs_path(new_file_name)?;
        self.filesystem
            .rename_with_options(&source, &target, replace_if_exists)
            .map_err(map_vfs_error)?;
        *context.path.write() = target;
        Ok(())
    }

    fn set_basic_info(
        &self,
        context: &Self::FileContext,
        file_attributes: u32,
        _creation_time: u64,
        _last_access_time: u64,
        _last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if file_attributes != INVALID_FILE_ATTRIBUTES {
            self.filesystem
                .set_metadata(
                    &context.path.read(),
                    MetadataChanges {
                        len: None,
                        readonly: Some(file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0),
                    },
                )
                .map_err(map_vfs_error)?;
            context.metadata.write().readonly = file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0;
        }
        *file_info = self.context_file_info(context)?;
        Ok(())
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> winfsp::Result<()> {
        if delete_file {
            let path = context.path.read();
            let kind = match context.kind {
                FileKind::File => RemoveKind::File,
                FileKind::Directory => RemoveKind::Directory,
            };
            self.filesystem
                .validate_remove(&path, kind)
                .map_err(map_vfs_error)?;
        }
        context
            .delete_pending
            .store(delete_file, AtomicOrdering::Release);
        Ok(())
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if !context.can_write {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        let current = self.context_metadata(context)?;
        if !set_allocation_size || new_size < current.len {
            context
                .file
                .as_ref()
                .ok_or_else(|| FspError::from(STATUS_FILE_IS_A_DIRECTORY))?
                .truncate(new_size)
                .map_err(map_vfs_error)?;
        }
        *file_info = self.context_file_info(context)?;
        Ok(())
    }

    fn read(
        &self,
        context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> winfsp::Result<u32> {
        #[cfg(test)]
        {
            TEST_READ_CALLS.fetch_add(1, AtomicOrdering::Relaxed);
            TEST_READ_BYTES.fetch_add(buffer.len() as u64, AtomicOrdering::Relaxed);
        }
        // Windows has already checked the user handle's granted access. Cached
        // write-only handles can issue paging reads here, so rechecking the
        // original user access would incorrectly fail Cache Manager I/O.
        let read = context
            .file
            .as_ref()
            .ok_or_else(|| FspError::from(STATUS_FILE_IS_A_DIRECTORY))?
            .read(offset, buffer)
            .map_err(map_vfs_error)?;
        if read == 0 && !buffer.is_empty() {
            return Err(STATUS_END_OF_FILE.into());
        }
        u32::try_from(read).map_err(|_| STATUS_INVALID_DEVICE_REQUEST.into())
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        if !context.can_write {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        let metadata = self.context_metadata(context)?;
        let offset = if write_to_eof { metadata.len } else { offset };
        let buffer = if constrained_io {
            if offset >= metadata.len {
                &[]
            } else {
                &buffer[..buffer.len().min((metadata.len - offset) as usize)]
            }
        } else {
            buffer
        };
        let written = context
            .file
            .as_ref()
            .ok_or_else(|| FspError::from(STATUS_FILE_IS_A_DIRECTORY))?
            .write(offset, buffer)
            .map_err(map_vfs_error)?;
        *file_info = self.context_file_info(context)?;
        u32::try_from(written).map_err(|_| STATUS_INVALID_DEVICE_REQUEST.into())
    }

    fn get_volume_info(&self, volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        volume_info.total_size = VOLUME_SIZE;
        volume_info.free_size = VOLUME_SIZE;
        volume_info.set_volume_label("Snowglass");
        Ok(())
    }
}

fn vfs_path(file_name: &U16CStr) -> winfsp::Result<VfsPathBuf> {
    let mut units = file_name.as_slice().to_vec();
    if units.is_empty() {
        units.push(b'/' as u16);
    }
    for unit in &mut units {
        if *unit == b'\\' as u16 {
            *unit = b'/' as u16;
        }
    }
    VfsPathBuf::from_units(units).map_err(map_vfs_error)
}

fn to_file_info(metadata: &Metadata, path: &VfsPathBuf, timestamp: u64) -> FileInfo {
    FileInfo {
        file_attributes: file_attributes(metadata),
        reparse_tag: 0,
        allocation_size: metadata.len.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT,
        file_size: metadata.len,
        creation_time: timestamp,
        last_access_time: timestamp,
        last_write_time: timestamp,
        change_time: timestamp,
        index_number: path_index(path),
        hard_links: 0,
        ea_size: 0,
    }
}

fn file_attributes(metadata: &Metadata) -> u32 {
    let mut attributes = match metadata.kind {
        FileKind::File => FILE_ATTRIBUTE_ARCHIVE.0,
        FileKind::Directory => FILE_ATTRIBUTE_DIRECTORY.0,
    };
    if metadata.readonly {
        attributes |= FILE_ATTRIBUTE_READONLY.0;
    }
    attributes
}

fn path_index(path: &VfsPathBuf) -> u64 {
    path.as_slice()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, unit| {
            (hash ^ u64::from(*unit)).wrapping_mul(0x1000_0000_01b3)
        })
}

fn map_vfs_error(error: VfsError) -> FspError {
    match error {
        VfsError::InvalidPath { .. } | VfsError::InvalidPermissions(_) => {
            STATUS_OBJECT_NAME_INVALID.into()
        }
        VfsError::PermissionDenied(_) | VfsError::AuthorityExceeded { .. } => {
            STATUS_ACCESS_DENIED.into()
        }
        VfsError::NotFound(_) => STATUS_OBJECT_NAME_NOT_FOUND.into(),
        VfsError::AlreadyExists(_)
        | VfsError::BindingBelowFile(_)
        | VfsError::DuplicateBinding(_)
        | VfsError::NameCollision(_) => STATUS_OBJECT_NAME_COLLISION.into(),
        VfsError::NotDirectory(_) => STATUS_NOT_A_DIRECTORY.into(),
        VfsError::IsDirectory(_) => STATUS_FILE_IS_A_DIRECTORY.into(),
        VfsError::DirectoryNotEmpty(_) => STATUS_DIRECTORY_NOT_EMPTY.into(),
        VfsError::CrossDevice => STATUS_NOT_SAME_DEVICE.into(),
        VfsError::StaleNode => STATUS_FILE_INVALID.into(),
        VfsError::Asar(_) => STATUS_FILE_CORRUPT_ERROR.into(),
        VfsError::Io(error) => error.into(),
        VfsError::Unsupported(_) => STATUS_INVALID_DEVICE_REQUEST.into(),
    }
}

fn host_error(operation: &str, error: impl std::fmt::Debug) -> VfsError {
    VfsError::Unsupported(format!("WinFSP failed to {operation}: {error:?}"))
}

fn system_time_filetime() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_secs() * 10_000_000 + u64::from(now.subsec_nanos()) / 100 + 116_444_736_000_000_000
}

fn endpoint_serial(endpoint: &str) -> u32 {
    endpoint.encode_utf16().fold(0x811c_9dc5, |hash, unit| {
        (hash ^ u32::from(unit)).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_vfs::{
        FilesystemPlan, HostFileFactory, MemoryFileFactory, Permissions, Resource, ResourceOrigin,
        ScratchBacking, ScratchVolumeFactory,
    };
    use winfsp::U16CString;

    fn context() -> SnowglassFileSystem {
        let resource = Arc::new(Resource::new(
            Arc::new(ScratchVolumeFactory::new(ScratchBacking::Memory)),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_tree(VfsPathBuf::root(), resource, Permissions::ALL);
        let filesystem = Arc::new(plan.seal().unwrap());
        filesystem.materialize().unwrap();
        SnowglassFileSystem::new(filesystem).unwrap()
    }

    #[test]
    fn paths_are_converted_without_utf8_round_trips() {
        let path =
            U16CString::from_vec(vec![b'\\' as u16, 0xd800, b'.' as u16, b'b' as u16]).unwrap();
        let converted = vfs_path(&path).unwrap();
        assert_eq!(
            converted.as_slice(),
            &[b'/' as u16, 0xd800, b'.' as u16, b'b' as u16]
        );
    }

    #[test]
    fn callback_context_reads_writes_and_enumerates_nodes() {
        let context = context();
        let path: VfsPathBuf = "/User/file.bin".parse().unwrap();
        context
            .filesystem
            .create_dir(&"/User".parse().unwrap())
            .unwrap();
        context
            .filesystem
            .create_file(&path, CreateOptions { exclusive: true })
            .unwrap();
        let handle = context
            .open_path(
                path.clone(),
                FILE_NON_DIRECTORY_FILE,
                FILE_READ_DATA | FILE_WRITE_DATA,
            )
            .unwrap();
        let file = handle.file.as_ref().unwrap();
        file.write(0, b"snowglass").unwrap();
        let mut bytes = [0; 9];
        file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"snowglass");

        let entries = context
            .directory_entries(&"/User".parse().unwrap())
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[2].name,
            "file.bin".encode_utf16().collect::<Vec<_>>()
        );
    }

    #[test]
    #[ignore = "requires the WinFSP driver and briefly registers a random UNC prefix"]
    fn mounts_and_unmounts_a_unc_endpoint() {
        let scratch = Arc::new(Resource::new(
            Arc::new(ScratchVolumeFactory::new(ScratchBacking::Memory)),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        ));
        let readonly = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(b"mounted file".to_vec())),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_tree(VfsPathBuf::root(), scratch, Permissions::ALL);
        plan.mount_file(
            VfsPathBuf::new("/readonly.bin").unwrap(),
            readonly,
            Permissions::READ,
        );
        let filesystem = Arc::new(plan.seal().unwrap());
        let endpoint = FilesystemEndpoint::random();
        let user_path = VfsPathBuf::new("/User").unwrap();
        let source_path = VfsPathBuf::new("/User/bridge.txt").unwrap();
        let target_path = VfsPathBuf::new("/User/renamed.txt").unwrap();
        let readonly_path = VfsPathBuf::new("/readonly.bin").unwrap();
        let user_path = endpoint.project(user_path.as_path());
        let source_path = endpoint.project(source_path.as_path());
        let target_path = endpoint.project(target_path.as_path());
        let readonly_path = endpoint.project(readonly_path.as_path());
        let mounted = WinFspFilesystemHost::default()
            .mount(endpoint, filesystem)
            .unwrap();
        std::fs::create_dir(&user_path).unwrap();
        std::fs::write(&source_path, b"through WinFSP").unwrap();
        std::fs::rename(&source_path, &target_path).unwrap();
        assert_eq!(std::fs::read(&target_path).unwrap(), b"through WinFSP");
        assert_eq!(std::fs::read(&readonly_path).unwrap(), b"mounted file");
        assert!(std::fs::write(&readonly_path, b"denied").is_err());
        std::fs::remove_file(&target_path).unwrap();
        std::fs::remove_dir(&user_path).unwrap();
        drop(mounted);
    }

    #[test]
    #[ignore = "requires WinFSP and SNOWGLASS_VFS_BENCH_FILE pointing at a large file"]
    fn measures_host_file_callback_traffic() {
        use std::io::Read;
        use std::os::windows::fs::FileExt;
        use std::time::Instant;

        fn sequential_read(path: &str) -> (u64, std::time::Duration) {
            let mut file = std::fs::File::open(path).unwrap();
            let mut buffer = vec![0; 1024 * 1024];
            let mut bytes = 0;
            let started = Instant::now();
            loop {
                let read = file.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                bytes += read as u64;
            }
            (bytes, started.elapsed())
        }

        fn parallel_sequential_read(path: &str, workers: usize) -> (u64, std::time::Duration) {
            let started = Instant::now();
            let bytes = std::thread::scope(|scope| {
                let tasks = (0..workers)
                    .map(|_| scope.spawn(|| sequential_read(path).0))
                    .collect::<Vec<_>>();
                tasks.into_iter().map(|task| task.join().unwrap()).sum()
            });
            (bytes, started.elapsed())
        }

        fn parallel_random_reads(path: &str, workers: usize, reads: u64) -> std::time::Duration {
            let started = Instant::now();
            std::thread::scope(|scope| {
                let tasks = (0..workers)
                    .map(|worker| {
                        scope.spawn(move || {
                            let file = std::fs::File::open(path).unwrap();
                            let len = file.metadata().unwrap().len();
                            let mut buffer = [0; 4096];
                            let mut random = 0x9e37_79b9_7f4a_7c15_u64 ^ worker as u64;
                            for _ in 0..reads {
                                random ^= random << 7;
                                random ^= random >> 9;
                                let offset = random % (len - buffer.len() as u64);
                                file.seek_read(&mut buffer, offset).unwrap();
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                for task in tasks {
                    task.join().unwrap();
                }
            });
            started.elapsed()
        }

        let source = std::env::var_os("SNOWGLASS_VFS_BENCH_FILE")
            .map(std::path::PathBuf::from)
            .expect("SNOWGLASS_VFS_BENCH_FILE must point at a large file");
        let memory_resource = Arc::new(Resource::new(
            Arc::new(MemoryFileFactory::immutable(
                std::fs::read(&source).unwrap(),
            )),
            Permissions::READ,
            ResourceOrigin::Memory,
        ));
        let resource = Arc::new(Resource::new(
            Arc::new(HostFileFactory::new(source.clone(), false)),
            Permissions::READ,
            ResourceOrigin::HostFile,
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_file(
            VfsPathBuf::new("/sample.bin").unwrap(),
            resource,
            Permissions::READ,
        );
        plan.mount_file(
            VfsPathBuf::new("/memory.bin").unwrap(),
            memory_resource,
            Permissions::READ,
        );
        let filesystem = Arc::new(plan.seal().unwrap());
        let endpoint = FilesystemEndpoint::random();
        let path = endpoint.project(VfsPathBuf::new("/sample.bin").unwrap().as_path());
        let memory_path = endpoint.project(VfsPathBuf::new("/memory.bin").unwrap().as_path());
        let mounted = WinFspFilesystemHost::default()
            .mount(endpoint, filesystem)
            .unwrap();

        let _ = sequential_read(&path);
        TEST_READ_CALLS.store(0, AtomicOrdering::Relaxed);
        TEST_READ_BYTES.store(0, AtomicOrdering::Relaxed);
        let (bytes, elapsed) = sequential_read(&path);
        let calls = TEST_READ_CALLS.load(AtomicOrdering::Relaxed);
        let callback_bytes = TEST_READ_BYTES.load(AtomicOrdering::Relaxed);

        let mut repeated_file = std::fs::File::open(&path).unwrap();
        let mut repeated_buffer = vec![0; 1024 * 1024];
        while repeated_file.read(&mut repeated_buffer).unwrap() != 0 {}
        std::io::Seek::seek(&mut repeated_file, std::io::SeekFrom::Start(0)).unwrap();
        TEST_READ_CALLS.store(0, AtomicOrdering::Relaxed);
        TEST_READ_BYTES.store(0, AtomicOrdering::Relaxed);
        let repeated_started = Instant::now();
        while repeated_file.read(&mut repeated_buffer).unwrap() != 0 {}
        let repeated_elapsed = repeated_started.elapsed();
        let repeated_calls = TEST_READ_CALLS.load(AtomicOrdering::Relaxed);

        TEST_READ_CALLS.store(0, AtomicOrdering::Relaxed);
        TEST_READ_BYTES.store(0, AtomicOrdering::Relaxed);
        let file = std::fs::File::open(&path).unwrap();
        let len = file.metadata().unwrap().len();
        let mut buffer = [0; 4096];
        let mut random = 0x9e37_79b9_7f4a_7c15_u64;
        let random_started = Instant::now();
        for _ in 0..5_000 {
            random ^= random << 7;
            random ^= random >> 9;
            let offset = random % (len - buffer.len() as u64);
            file.seek_read(&mut buffer, offset).unwrap();
        }
        let random_elapsed = random_started.elapsed();
        let random_calls = TEST_READ_CALLS.load(AtomicOrdering::Relaxed);
        let random_callback_bytes = TEST_READ_BYTES.load(AtomicOrdering::Relaxed);

        let _ = sequential_read(&memory_path);
        TEST_READ_CALLS.store(0, AtomicOrdering::Relaxed);
        TEST_READ_BYTES.store(0, AtomicOrdering::Relaxed);
        let (memory_bytes, memory_elapsed) = sequential_read(&memory_path);
        let memory_calls = TEST_READ_CALLS.load(AtomicOrdering::Relaxed);
        let memory_file = std::fs::File::open(&memory_path).unwrap();
        let memory_len = memory_file.metadata().unwrap().len();
        let mut memory_buffer = [0; 4096];
        let mut memory_random = 0x9e37_79b9_7f4a_7c15_u64;
        let memory_random_started = Instant::now();
        for _ in 0..5_000 {
            memory_random ^= memory_random << 7;
            memory_random ^= memory_random >> 9;
            let offset = memory_random % (memory_len - memory_buffer.len() as u64);
            memory_file.seek_read(&mut memory_buffer, offset).unwrap();
        }
        let memory_random_elapsed = memory_random_started.elapsed();

        let native_path = source.to_string_lossy();
        let (native_parallel_bytes, native_parallel_elapsed) =
            parallel_sequential_read(&native_path, 8);
        TEST_READ_CALLS.store(0, AtomicOrdering::Relaxed);
        TEST_READ_BYTES.store(0, AtomicOrdering::Relaxed);
        let (parallel_bytes, parallel_elapsed) = parallel_sequential_read(&path, 8);
        let parallel_calls = TEST_READ_CALLS.load(AtomicOrdering::Relaxed);

        let native_parallel_random = parallel_random_reads(&native_path, 8, 5_000);
        TEST_READ_CALLS.store(0, AtomicOrdering::Relaxed);
        TEST_READ_BYTES.store(0, AtomicOrdering::Relaxed);
        let parallel_random = parallel_random_reads(&path, 8, 5_000);
        let parallel_random_calls = TEST_READ_CALLS.load(AtomicOrdering::Relaxed);

        eprintln!(
            "WinFSP callback traffic: sequential {:.1} MiB/s, {calls} callbacks/{callback_bytes} bytes; random {:.0} IOPS, {random_calls} callbacks/{random_callback_bytes} bytes",
            bytes as f64 / 1_048_576.0 / elapsed.as_secs_f64(),
            5_000.0 / random_elapsed.as_secs_f64(),
        );
        eprintln!(
            "Same-handle second pass: {:.1} MiB/s ({repeated_calls} callbacks)",
            bytes as f64 / 1_048_576.0 / repeated_elapsed.as_secs_f64(),
        );
        eprintln!(
            "WinFSP memory ceiling: sequential {:.1} MiB/s ({memory_calls} callbacks), random {:.0} IOPS",
            memory_bytes as f64 / 1_048_576.0 / memory_elapsed.as_secs_f64(),
            5_000.0 / memory_random_elapsed.as_secs_f64(),
        );
        eprintln!(
            "8-way traffic: native sequential {:.1} MiB/s, WinFSP sequential {:.1} MiB/s ({parallel_calls} callbacks); native random {:.0} IOPS, WinFSP random {:.0} IOPS ({parallel_random_calls} callbacks)",
            native_parallel_bytes as f64 / 1_048_576.0 / native_parallel_elapsed.as_secs_f64(),
            parallel_bytes as f64 / 1_048_576.0 / parallel_elapsed.as_secs_f64(),
            40_000.0 / native_parallel_random.as_secs_f64(),
            40_000.0 / parallel_random.as_secs_f64(),
        );
        drop(mounted);
    }
}
