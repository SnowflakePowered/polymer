use dashmap::DashMap;
use parking_lot::RwLock;
use polymer_vfs::{
    CreateOptions, FileKind, FilesystemEndpoint, FilesystemHost, FinalizationReason, Metadata,
    MetadataChanges, MountedFilesystem, OpenFile, OpenOptions, PreparedFilesystem, RemoveKind,
    VfsError, VfsPathBuf, VfsPathUnit, VfsResult,
};
use std::{
    ffi::c_void,
    sync::{Arc, OnceLock, Weak},
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
            FILE_ATTRIBUTE_SPARSE_FILE, INVALID_FILE_ATTRIBUTES,
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
            let context = PolymerFileSystem::new(filesystem.clone())
                .map_err(|error| host_error("build security descriptor", error))?;
            let root = endpoint.project(VfsPathBuf::root().as_path());
            let prefix = root.strip_prefix('\\').ok_or_else(|| {
                VfsError::Unsupported(format!("invalid WinFSP endpoint '{root}'"))
            })?;
            let mut parameters = VolumeParams::new();
            parameters
                .prefix(prefix)
                .filesystem_name("Polymer")
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
                .flush_and_purge_on_cleanup(true)
                .post_disposition_only_when_necessary(true)
                .supports_posix_unlink_rename(true);
            let mut host: NativeFileSystemHost<PolymerFileSystem> =
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

struct PolymerFileSystem {
    filesystem: Arc<PreparedFilesystem>,
    security: SecurityDescriptor,
    created_at: u64,
    paths: DashMap<Vec<Vec<VfsPathUnit>>, Weak<RwLock<VfsPathBuf>>>,
}

struct SecurityDescriptor(Vec<u8>);

struct PolymerFileContext {
    path: Arc<RwLock<VfsPathBuf>>,
    kind: FileKind,
    metadata: RwLock<Metadata>,
    file: RwLock<Option<Box<dyn OpenFile>>>,
    directory_buffer: OnceLock<DirBuffer>,
    can_write: bool,
}

struct ListedEntry {
    name: Vec<u16>,
    path: VfsPathBuf,
    metadata: Metadata,
}

impl PolymerFileSystem {
    fn new(filesystem: Arc<PreparedFilesystem>) -> winfsp::Result<Self> {
        Ok(Self {
            filesystem,
            security: SecurityDescriptor::full_access()?,
            created_at: system_time_filetime(),
            paths: DashMap::new(),
        })
    }

    fn move_paths(&self, source: &VfsPathBuf, target: &VfsPathBuf) -> winfsp::Result<()> {
        let from = source.normalized_components().map_err(map_vfs_error)?;
        let to = target.normalized_components().map_err(map_vfs_error)?;
        let moved = self
            .paths
            .iter()
            .filter(|entry| entry.key().starts_with(&from))
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        for key in moved {
            let Some((key, shared)) = self.paths.remove(&key) else {
                continue;
            };
            let Some(shared) = shared.upgrade() else {
                continue;
            };
            let mut path = shared.write();
            let mut units = target.as_slice().to_vec();
            for component in path.components().skip(from.len()) {
                units.push(b'/' as u16);
                units.extend_from_slice(component.as_slice());
            }
            *path = VfsPathBuf::from_units(units).map_err(map_vfs_error)?;
            let renamed = [to.as_slice(), &key[from.len()..]].concat();
            self.paths.insert(renamed, Arc::downgrade(&shared));
        }
        Ok(())
    }

    fn open_path(
        &self,
        path: VfsPathBuf,
        create_options: u32,
        granted_access: u32,
    ) -> winfsp::Result<PolymerFileContext> {
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
        let key = path.normalized_components().map_err(map_vfs_error)?;
        let mut entry = self.paths.entry(key).or_default();
        let path = entry.upgrade().unwrap_or_else(|| {
            let shared = Arc::new(RwLock::new(path));
            *entry = Arc::downgrade(&shared);
            shared
        });
        drop(entry);
        Ok(PolymerFileContext {
            path,
            kind: metadata.kind,
            metadata: RwLock::new(metadata),
            file: RwLock::new(file),
            directory_buffer: OnceLock::new(),
            can_write,
        })
    }

    fn context_metadata(&self, context: &PolymerFileContext) -> winfsp::Result<Metadata> {
        match context.kind {
            FileKind::File => self.with_file(context, |file| file.metadata()),
            FileKind::Directory => Ok(context.metadata.read().clone()),
        }
    }

    fn with_file<T>(
        &self,
        context: &PolymerFileContext,
        operation: impl FnOnce(&dyn OpenFile) -> polymer_vfs::VfsResult<T>,
    ) -> winfsp::Result<T> {
        if context.kind != FileKind::File {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        let file = context.file.upgradable_read();
        let file = match file.as_deref() {
            Some(_) => parking_lot::RwLockUpgradableReadGuard::downgrade(file),
            None => {
                let mut file = parking_lot::RwLockUpgradableReadGuard::upgrade(file);
                let reopened = self
                    .filesystem
                    .open(
                        &context.path.read(),
                        OpenOptions {
                            read: true,
                            write: context.can_write,
                            truncate: false,
                        },
                    )
                    .map_err(map_vfs_error)?;
                *file = Some(reopened);
                parking_lot::RwLockWriteGuard::downgrade(file)
            }
        };
        operation(file.as_deref().expect("file contexts hold an open file")).map_err(map_vfs_error)
    }

    fn context_file_info(&self, context: &PolymerFileContext) -> winfsp::Result<FileInfo> {
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

impl FileSystemContext for PolymerFileSystem {
    type FileContext = PolymerFileContext;

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

    fn close(&self, context: Self::FileContext) {
        let key = context.path.read().normalized_components();
        drop(context);
        if let Ok(key) = key {
            self.paths.remove_if(&key, |_, shared| shared.strong_count() == 0);
        }
    }

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
            if let Some(file) = context.file.read().as_deref() {
                let _ = file.set_sparse();
            }
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
        if let Some(file) = context.file.write().take() {
            let _ = file.flush();
        }
        if FspCleanupFlags::FspCleanupDelete.is_flagged(flags) {
            let path = context.path.read().clone();
            if let Ok(key) = path.normalized_components() {
                self.paths.remove(&key);
            }
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
        if context.kind == FileKind::File {
            self.with_file(context, |file| file.sync())?;
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
        self.with_file(context, |file| file.truncate(0))?;
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
        self.move_paths(&source, &target)?;
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
            self.with_file(context, |file| file.truncate(new_size))?;
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
        // Windows has already checked the user handle's granted access. Cached
        // write-only handles can issue paging reads here, so rechecking the
        // original user access would incorrectly fail Cache Manager I/O.
        let read = self.with_file(context, |file| file.read(offset, buffer))?;
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
        let written = self.with_file(context, |file| file.write(offset, buffer))?;
        *file_info = self.context_file_info(context)?;
        u32::try_from(written).map_err(|_| STATUS_INVALID_DEVICE_REQUEST.into())
    }

    fn get_volume_info(&self, volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        volume_info.total_size = VOLUME_SIZE;
        volume_info.free_size = VOLUME_SIZE;
        volume_info.set_volume_label("Polymer");
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
    if metadata.sparse {
        attributes |= FILE_ATTRIBUTE_SPARSE_FILE.0;
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
