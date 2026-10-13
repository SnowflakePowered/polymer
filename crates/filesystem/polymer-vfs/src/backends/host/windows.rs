use super::HostOpen;
use crate::{
    CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind, FileNode, Metadata,
    MetadataChanges, Node, OpenFile, OpenOptions, RemoveKind, VfsComponent, VfsError, VfsPath,
    VfsPathBuf, VfsResult, Volume,
};
use parking_lot::Mutex;
use std::{
    fs::File,
    mem::{offset_of, size_of, size_of_val},
    os::windows::{fs::MetadataExt, io::AsRawHandle, io::FromRawHandle},
    path::Path,
    ptr,
    sync::Arc,
};
use windows::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
            FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
            FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation, FileRenameInformationEx,
            NTCREATEFILE_CREATE_DISPOSITION, NTCREATEFILE_CREATE_OPTIONS, NtCreateFile,
            NtSetInformationFile, RtlNtStatusToDosErrorNoTeb,
        },
    },
    Win32::{
        Foundation::{
            ERROR_NO_MORE_FILES, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, NTSTATUS,
            OBJ_CASE_INSENSITIVE, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
            STATUS_OBJECT_PATH_NOT_FOUND, UNICODE_STRING,
        },
        Storage::FileSystem::{
            CreateFileW, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_ATTRIBUTE_SPARSE_FILE, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
            FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS,
            FILE_END_OF_FILE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            FILE_ID_BOTH_DIR_INFO, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx,
            FileEndOfFileInfo, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
            GetFileInformationByHandleEx, OPEN_EXISTING, ReOpenFile, SetFileInformationByHandle,
        },
        System::IO::IO_STATUS_BLOCK,
    },
    core::{HSTRING, PWSTR},
};

const SHARE_ALL: FILE_SHARE_MODE =
    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);

pub(super) struct HostFile {
    handle: Arc<File>,
    writable: bool,
}

impl HostFile {
    pub(super) fn new(path: &Path, writable: bool) -> VfsResult<Self> {
        let handle = open_root(path, writable, false)?;
        require_regular_file(&handle)?;
        Ok(Self {
            handle: Arc::new(handle),
            writable,
        })
    }
}

impl FileNode for HostFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        metadata_for(&self.handle, self.writable)
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        set_handle_metadata(&self.handle, self.writable, changes, false)
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        open_captured(&self.handle, self.writable, options)
    }
}

pub(super) struct HostVolume {
    inner: Arc<HostVolumeInner>,
}

struct HostVolumeInner {
    root: Arc<File>,
    writable: bool,
}

struct HostDirectory {
    volume: Arc<HostVolumeInner>,
    handle: Arc<File>,
    enumeration: Mutex<()>,
}

struct HostTreeFile {
    volume: Arc<HostVolumeInner>,
    handle: Arc<File>,
}

struct CapturedNode {
    handle: Arc<File>,
    kind: FileKind,
}

struct HostDirectoryEntry {
    name: VfsComponent,
    metadata: Metadata,
}

impl HostVolume {
    pub(super) fn new(root: &Path, writable: bool, _internal_links: bool) -> VfsResult<Self> {
        let root = open_root(root, writable, true)?;
        require_directory(&root)?;
        Ok(Self {
            inner: Arc::new(HostVolumeInner {
                root: Arc::new(root),
                writable,
            }),
        })
    }

    fn component(path: &VfsPath) -> VfsResult<VfsComponent> {
        path.components()
            .next_back()
            .map(|name| VfsComponent::from_units(crate::PathStr::as_slice(name).to_vec()))
            .transpose()?
            .ok_or_else(|| VfsError::Unsupported("the volume root has no name".into()))
    }
}

impl HostVolumeInner {
    fn node(self: &Arc<Self>, handle: File) -> VfsResult<Node> {
        reject_reparse_point(&handle)?;
        let metadata = handle.metadata()?;
        let handle = Arc::new(handle);
        if metadata.is_dir() {
            Ok(Node::Directory(Arc::new(HostDirectory {
                volume: self.clone(),
                handle,
                enumeration: Mutex::new(()),
            })))
        } else if metadata.is_file() {
            Ok(Node::File(Arc::new(HostTreeFile {
                volume: self.clone(),
                handle,
            })))
        } else {
            Err(VfsError::Unsupported(
                "host volume exposes only files and directories".into(),
            ))
        }
    }

    fn open_child(self: &Arc<Self>, parent: &File, name: &VfsComponent) -> VfsResult<Option<Node>> {
        match nt_open_relative(
            parent,
            name.as_slice(),
            desired_access(self.writable),
            FILE_OPEN,
            FILE_OPEN_FOR_BACKUP_INTENT | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        ) {
            Ok(handle) => self.node(handle).map(Some),
            Err(status)
                if status == STATUS_OBJECT_NAME_NOT_FOUND
                    || status == STATUS_OBJECT_PATH_NOT_FOUND =>
            {
                Ok(None)
            }
            Err(status) => Err(nt_error(status)),
        }
    }

    fn resolve(self: &Arc<Self>, path: &VfsPath) -> VfsResult<CapturedNode> {
        let mut current = CapturedNode {
            handle: self.root.clone(),
            kind: FileKind::Directory,
        };
        for component in path.components() {
            if current.kind != FileKind::Directory {
                return Err(VfsError::NotDirectory(VfsPathBuf::root()));
            }
            let name: VfsComponent =
                VfsComponent::from_units(crate::PathStr::as_slice(component).to_vec())?;
            let handle = match nt_open_relative(
                &current.handle,
                name.as_slice(),
                desired_access(self.writable),
                FILE_OPEN,
                FILE_OPEN_FOR_BACKUP_INTENT
                    | FILE_OPEN_REPARSE_POINT
                    | FILE_SYNCHRONOUS_IO_NONALERT,
            ) {
                Ok(handle) => handle,
                Err(status)
                    if status == STATUS_OBJECT_NAME_NOT_FOUND
                        || status == STATUS_OBJECT_PATH_NOT_FOUND =>
                {
                    return Err(VfsError::NotFound(path.to_owned()));
                }
                Err(status) => return Err(nt_error(status)),
            };
            reject_reparse_point(&handle)?;
            let metadata = handle.metadata()?;
            let kind = if metadata.is_dir() {
                FileKind::Directory
            } else if metadata.is_file() {
                FileKind::File
            } else {
                return Err(VfsError::Unsupported(
                    "host volume exposes only files and directories".into(),
                ));
            };
            current = CapturedNode {
                handle: Arc::new(handle),
                kind,
            };
        }
        Ok(current)
    }
}

impl Volume for HostVolume {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>> {
        Ok(Arc::new(HostDirectory {
            volume: self.inner.clone(),
            handle: self.inner.root.clone(),
            enumeration: Mutex::new(()),
        }))
    }

    fn rename(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        self.rename_inner(source, target, false)
    }

    fn replace(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        self.rename_inner(source, target, true)
    }

    fn detach(&self, source: &VfsPath) -> VfsResult<()> {
        VfsError::require_writable(self.inner.writable)?;
        let source = self.inner.resolve(source)?;
        if source.kind != FileKind::File {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        }
        detach_handle(&source.handle)
    }
}

impl HostVolume {
    fn rename_inner(
        &self,
        source: &VfsPath,
        target: &VfsPath,
        replace_if_exists: bool,
    ) -> VfsResult<()> {
        VfsError::require_writable(self.inner.writable)?;
        let source = self.inner.resolve(source)?;
        let target_parent = target
            .parent()
            .ok_or_else(|| VfsError::Unsupported("the volume root cannot be replaced".into()))?;
        let target_parent = self.inner.resolve(target_parent.as_path())?;
        if target_parent.kind != FileKind::Directory {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        }
        rename_handle(
            &source.handle,
            &target_parent.handle,
            &Self::component(target)?,
            replace_if_exists,
        )
    }
}

impl DirectoryNode for HostDirectory {
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            kind: FileKind::Directory,
            len: 0,
            readonly: !self.volume.writable,
            sparse: false,
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        set_handle_metadata(&self.handle, self.volume.writable, changes, true)
    }

    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>> {
        self.volume.open_child(&self.handle, name)
    }

    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>> {
        let _enumeration = self.enumeration.lock();
        let entries = directory_entries(&self.handle, self.volume.writable)?
            .into_iter()
            .map(|entry| {
                Ok(DirectoryEntry {
                    name: entry.name,
                    metadata: entry.metadata,
                })
            })
            .collect::<Vec<_>>();
        Ok(Box::new(entries.into_iter()))
    }

    fn create_file(
        &self,
        name: &VfsComponent,
        options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>> {
        VfsError::require_writable(self.volume.writable)?;
        let disposition = if options.exclusive {
            FILE_CREATE
        } else {
            FILE_OPEN_IF
        };
        let handle = match nt_open_relative(
            &self.handle,
            name.as_slice(),
            desired_access(true),
            disposition,
            FILE_NON_DIRECTORY_FILE
                | FILE_OPEN_FOR_BACKUP_INTENT
                | FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT,
        ) {
            Ok(handle) => handle,
            Err(status) if status == STATUS_OBJECT_NAME_COLLISION => {
                return Err(VfsError::AlreadyExists(VfsPathBuf::root()));
            }
            Err(status) => return Err(nt_error(status)),
        };
        reject_reparse_point(&handle)?;
        require_regular_file(&handle)?;
        Ok(Arc::new(HostTreeFile {
            volume: self.volume.clone(),
            handle: Arc::new(handle),
        }))
    }

    fn create_dir(&self, name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>> {
        VfsError::require_writable(self.volume.writable)?;
        let handle = match nt_open_relative(
            &self.handle,
            name.as_slice(),
            desired_access(true),
            FILE_CREATE,
            FILE_DIRECTORY_FILE
                | FILE_OPEN_FOR_BACKUP_INTENT
                | FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT,
        ) {
            Ok(handle) => handle,
            Err(status) if status == STATUS_OBJECT_NAME_COLLISION => {
                return Err(VfsError::AlreadyExists(VfsPathBuf::root()));
            }
            Err(status) => return Err(nt_error(status)),
        };
        reject_reparse_point(&handle)?;
        require_directory(&handle)?;
        Ok(Arc::new(HostDirectory {
            volume: self.volume.clone(),
            handle: Arc::new(handle),
            enumeration: Mutex::new(()),
        }))
    }

    fn remove(&self, name: &VfsComponent, kind: RemoveKind) -> VfsResult<()> {
        VfsError::require_writable(self.volume.writable)?;
        let handle = match nt_open_relative(
            &self.handle,
            name.as_slice(),
            desired_access(true),
            FILE_OPEN,
            FILE_OPEN_FOR_BACKUP_INTENT | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        ) {
            Ok(handle) => handle,
            Err(status)
                if status == STATUS_OBJECT_NAME_NOT_FOUND
                    || status == STATUS_OBJECT_PATH_NOT_FOUND =>
            {
                return Err(VfsError::NotFound(VfsPathBuf::root()));
            }
            Err(status) => return Err(nt_error(status)),
        };
        reject_reparse_point(&handle)?;
        let metadata = handle.metadata()?;
        let actual_kind = if metadata.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
        };
        if actual_kind
            != match kind {
                RemoveKind::File => FileKind::File,
                RemoveKind::Directory => FileKind::Directory,
            }
        {
            return match kind {
                RemoveKind::File => Err(VfsError::IsDirectory(VfsPathBuf::root())),
                RemoveKind::Directory => Err(VfsError::NotDirectory(VfsPathBuf::root())),
            };
        }
        delete_handle(&handle)
    }
}

impl FileNode for HostTreeFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        metadata_for(&self.handle, self.volume.writable)
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        set_handle_metadata(&self.handle, self.volume.writable, changes, false)
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        open_captured(&self.handle, self.volume.writable, options)
    }
}

fn open_root(path: &Path, writable: bool, directory: bool) -> VfsResult<File> {
    let path = HSTRING::from(path.as_os_str());
    let access = GENERIC_READ.0 | if writable { GENERIC_WRITE.0 } else { 0 };
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            FILE_FLAGS_AND_ATTRIBUTES(0)
        };
    let handle = unsafe { CreateFileW(&path, access, SHARE_ALL, None, OPEN_EXISTING, flags, None) }
        .map_err(|_| std::io::Error::last_os_error())?;
    let file = unsafe { File::from_raw_handle(handle.0) };
    reject_reparse_point(&file)?;
    Ok(file)
}

fn nt_open_relative(
    parent: &File,
    name: &[u16],
    desired_access: FILE_ACCESS_RIGHTS,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<File, NTSTATUS> {
    let byte_len = u16::try_from(name.len().saturating_mul(size_of::<u16>()))
        .map_err(|_| windows::Win32::Foundation::STATUS_NAME_TOO_LONG)?;
    let unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: PWSTR(name.as_ptr().cast_mut()),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: raw_handle(parent),
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle = HANDLE::default();
    let mut io_status = IO_STATUS_BLOCK::default();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &attributes,
            &mut io_status,
            None,
            FILE_ATTRIBUTE_NORMAL,
            SHARE_ALL,
            disposition,
            options,
            None,
            0,
        )
    };
    if status.is_err() {
        return Err(status);
    }
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

fn desired_access(writable: bool) -> FILE_ACCESS_RIGHTS {
    FILE_ACCESS_RIGHTS(
        FILE_GENERIC_READ.0
            | if writable {
                FILE_GENERIC_WRITE.0 | DELETE.0
            } else {
                0
            },
    )
}

fn open_captured(
    source: &File,
    writable: bool,
    options: OpenOptions,
) -> VfsResult<Box<dyn OpenFile>> {
    if options.write || options.truncate {
        VfsError::require_writable(writable)?;
    }
    // Cached write-only Windows handles may generate paging reads. Keep the
    // captured backend handle readable; WinFSP still enforces the user-visible
    // granted access before dispatching ordinary reads.
    let access = GENERIC_READ.0 | if options.write { GENERIC_WRITE.0 } else { 0 };
    let handle = unsafe {
        ReOpenFile(
            raw_handle(source),
            access,
            SHARE_ALL,
            FILE_FLAGS_AND_ATTRIBUTES(0),
        )
    }
    .map_err(|_| std::io::Error::last_os_error())?;
    let file = unsafe { File::from_raw_handle(handle.0) };
    if options.truncate {
        file.set_len(0)?;
    }
    Ok(Box::new(HostOpen { file, writable }))
}

pub(super) fn metadata_for(file: &File, writable: bool) -> VfsResult<Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(VfsError::Unsupported(
            "host volume exposes only files and directories".into(),
        ));
    }
    Ok(Metadata {
        kind: FileKind::File,
        len: metadata.len(),
        readonly: !writable || metadata.file_attributes() & FILE_ATTRIBUTE_READONLY.0 != 0,
        sparse: metadata.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE.0 != 0,
    })
}

fn set_handle_metadata(
    file: &File,
    writable: bool,
    changes: MetadataChanges,
    directory: bool,
) -> VfsResult<()> {
    VfsError::require_writable(writable)?;
    if let Some(len) = changes.len {
        if directory {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        }
        let info = FILE_END_OF_FILE_INFO {
            EndOfFile: i64::try_from(len)
                .map_err(|_| VfsError::Unsupported("file length exceeds i64".into()))?,
        };
        set_file_information(file, FileEndOfFileInfo, &info)?;
    }
    if let Some(readonly) = changes.readonly {
        let mut info = FILE_BASIC_INFO::default();
        get_file_information(file, FileBasicInfo, &mut info)?;
        if readonly {
            info.FileAttributes |= FILE_ATTRIBUTE_READONLY.0;
        } else {
            info.FileAttributes &= !FILE_ATTRIBUTE_READONLY.0;
            if info.FileAttributes == 0 {
                info.FileAttributes = FILE_ATTRIBUTE_NORMAL.0;
            }
        }
        set_file_information(file, FileBasicInfo, &info)?;
    }
    Ok(())
}

fn require_regular_file(file: &File) -> VfsResult<()> {
    reject_reparse_point(file)?;
    if !file.metadata()?.is_file() {
        return Err(VfsError::IsDirectory(VfsPathBuf::root()));
    }
    Ok(())
}

fn require_directory(file: &File) -> VfsResult<()> {
    reject_reparse_point(file)?;
    if !file.metadata()?.is_dir() {
        return Err(VfsError::NotDirectory(VfsPathBuf::root()));
    }
    Ok(())
}

fn reject_reparse_point(file: &File) -> VfsResult<()> {
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(VfsError::Unsupported(
            "links are not exposed by confined host volumes".into(),
        ));
    }
    Ok(())
}

fn directory_entries(directory: &File, writable: bool) -> VfsResult<Vec<HostDirectoryEntry>> {
    const BUFFER_SIZE: usize = 64 * 1024;
    let mut storage = vec![0_u64; BUFFER_SIZE / size_of::<u64>()];
    let mut restart = true;
    let mut entries = Vec::new();
    loop {
        storage.fill(0);
        let class = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        let result = unsafe {
            GetFileInformationByHandleEx(
                raw_handle(directory),
                class,
                storage.as_mut_ptr().cast(),
                BUFFER_SIZE as u32,
            )
        };
        if result.is_err() {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(std::io::Error::from_raw_os_error(error.0 as i32).into());
        }
        restart = false;
        let bytes = storage.as_ptr().cast::<u8>();
        let mut offset = 0_usize;
        loop {
            if offset + offset_of!(FILE_ID_BOTH_DIR_INFO, FileName) > BUFFER_SIZE {
                return Err(VfsError::Unsupported(
                    "invalid directory information returned by Windows".into(),
                ));
            }
            let entry = unsafe { &*bytes.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>() };
            let name_len = entry.FileNameLength as usize / size_of::<u16>();
            let name_end =
                offset + offset_of!(FILE_ID_BOTH_DIR_INFO, FileName) + name_len * size_of::<u16>();
            if name_end > BUFFER_SIZE {
                return Err(VfsError::Unsupported(
                    "invalid directory name returned by Windows".into(),
                ));
            }
            if entry.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                return Err(VfsError::Unsupported(
                    "links are not exposed by confined host volumes".into(),
                ));
            }
            let name = unsafe {
                std::slice::from_raw_parts(ptr::addr_of!(entry.FileName).cast::<u16>(), name_len)
            };
            if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                let kind = if entry.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
                    FileKind::Directory
                } else {
                    FileKind::File
                };
                entries.push(HostDirectoryEntry {
                    name: VfsComponent::from_units(name.to_vec())?,
                    metadata: Metadata {
                        kind,
                        len: if kind == FileKind::File {
                            entry.EndOfFile as u64
                        } else {
                            0
                        },
                        readonly: !writable
                            || entry.FileAttributes & FILE_ATTRIBUTE_READONLY.0 != 0,
                        sparse: entry.FileAttributes & FILE_ATTRIBUTE_SPARSE_FILE.0 != 0,
                    },
                });
            }
            if entry.NextEntryOffset == 0 {
                break;
            }
            offset = offset
                .checked_add(entry.NextEntryOffset as usize)
                .ok_or_else(|| {
                    VfsError::Unsupported("directory information offset overflowed".into())
                })?;
        }
    }
    Ok(entries)
}

fn rename_handle(
    source: &File,
    target_parent: &File,
    name: &VfsComponent,
    replace_if_exists: bool,
) -> VfsResult<()> {
    let name_bytes = size_of_val(name.as_slice());
    let total = size_of::<FILE_RENAME_INFORMATION>() + name_bytes;
    let mut storage = vec![0_u64; total.div_ceil(size_of::<u64>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let mut io_status = IO_STATUS_BLOCK::default();
    unsafe {
        let information_class = if replace_if_exists {
            (*info).Anonymous.Flags = FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS;
            FileRenameInformationEx
        } else {
            (*info).Anonymous.ReplaceIfExists = false;
            FileRenameInformation
        };
        (*info).RootDirectory = raw_handle(target_parent);
        (*info).FileNameLength = name_bytes as u32;
        ptr::addr_of_mut!((*info).FileName)
            .cast::<u16>()
            .copy_from_nonoverlapping(name.as_slice().as_ptr(), name.as_slice().len());
        let status = NtSetInformationFile(
            raw_handle(source),
            &mut io_status,
            info.cast(),
            total as u32,
            information_class,
        );
        if status.is_err() {
            return Err(nt_error(status));
        }
    }
    Ok(())
}

fn delete_handle(file: &File) -> VfsResult<()> {
    let posix = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
        ),
    };
    set_file_information(file, FileDispositionInfoEx, &posix).or_else(|_| {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        set_file_information(file, FileDispositionInfo, &info)
    })
}

fn detach_handle(file: &File) -> VfsResult<()> {
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0,
        ),
    };
    set_file_information(file, FileDispositionInfoEx, &info)
}

fn get_file_information<T>(
    file: &File,
    class: windows::Win32::Storage::FileSystem::FILE_INFO_BY_HANDLE_CLASS,
    output: &mut T,
) -> VfsResult<()> {
    unsafe {
        GetFileInformationByHandleEx(
            raw_handle(file),
            class,
            ptr::from_mut(output).cast(),
            size_of::<T>() as u32,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())?;
    Ok(())
}

fn set_file_information<T>(
    file: &File,
    class: windows::Win32::Storage::FileSystem::FILE_INFO_BY_HANDLE_CLASS,
    input: &T,
) -> VfsResult<()> {
    unsafe {
        SetFileInformationByHandle(
            raw_handle(file),
            class,
            ptr::from_ref(input).cast(),
            size_of::<T>() as u32,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())?;
    Ok(())
}

fn raw_handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn nt_error(status: NTSTATUS) -> VfsError {
    let error = unsafe { RtlNtStatusToDosErrorNoTeb(status) };
    std::io::Error::from_raw_os_error(error as i32).into()
}
