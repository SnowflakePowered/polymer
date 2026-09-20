mod operations;
use super::{
    connection::{Connection, Error, Resource},
    item::Item,
};
use crate::wire::{Attributes, Entry, MAX_IO, Request, Response};
use dashmap::DashMap;
use objc2::{AnyThread, DefinedClass, define_class, msg_send, rc::Retained};
use objc2_foundation::{NSData, NSError, NSObjectProtocol};
use objc2_fs_kit::*;
use parking_lot::Mutex;
use std::{
    io, ptr,
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) struct State {
    connection: Mutex<Connection>,
    items: DashMap<Vec<u8>, Retained<Item>>,
    next: AtomicU64,
    // FSKit can call concurrently; serialize namespace changes with lookup and I/O.
    namespace: Mutex<()>,
    readonly: bool,
}

macro_rules! done {
    ($volume:expr, $reply:expr, $result:expr) => {
        match $volume.with_namespace(|| $result) {
            Ok(_) => $reply.call((ptr::null_mut(),)),
            Err(error) => $reply.call((Retained::as_ptr(&Error::native(error)).cast_mut(),)),
        }
    };
}
macro_rules! object_reply {
    ($volume:expr, $reply:expr, $result:expr) => {
        match $volume.with_namespace(|| $result) {
            Ok(object) => {
                $reply.call((Retained::as_ptr(&object).cast_mut().cast(), ptr::null_mut()))
            }
            Err(error) => $reply.call((
                ptr::null_mut(),
                Retained::as_ptr(&Error::native(error)).cast_mut(),
            )),
        }
    };
}
macro_rules! item_reply {
    ($volume:expr, $reply:expr, $result:expr, $name:expr) => {
        match $volume.with_namespace(|| $result) {
            Ok(object) => $reply.call((
                Retained::as_ptr(&object).cast_mut().cast(),
                $name as *const FSFileName as *mut FSFileName,
                ptr::null_mut(),
            )),
            Err(error) => $reply.call((
                ptr::null_mut(),
                ptr::null_mut(),
                Retained::as_ptr(&Error::native(error)).cast_mut(),
            )),
        }
    };
}

define_class!(
    #[unsafe(super(FSVolume))]
    #[ivars = State]
    pub(super) struct Volume;
    unsafe impl NSObjectProtocol for Volume {}
    unsafe impl FSVolumePathConfOperations for Volume {
        #[unsafe(method(maximumLinkCount))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn maximum_link_count(&self) -> isize {
            unsafe { 1 }
        }
        #[unsafe(method(maximumNameLength))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn maximum_name_length(&self) -> isize {
            unsafe { 255 }
        }
        #[unsafe(method(restrictsOwnershipChanges))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn restricts_ownership_changes(&self) -> bool {
            unsafe { true }
        }
        #[unsafe(method(truncatesLongNames))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn truncates_long_names(&self) -> bool {
            unsafe { false }
        }
        #[unsafe(method(maximumXattrSize))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn maximum_xattr_size(&self) -> isize {
            unsafe { 0 }
        }
        #[unsafe(method(maximumXattrSizeInBits))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn maximum_xattr_size_in_bits(&self) -> isize {
            unsafe { 0 }
        }
        #[unsafe(method(maximumFileSize))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn maximum_file_size(&self) -> u64 {
            unsafe { i64::MAX as u64 }
        }
        #[unsafe(method(maximumFileSizeInBits))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn maximum_file_size_in_bits(&self) -> isize {
            unsafe { 63 }
        }
    }
    unsafe impl FSVolumeOperations for Volume {
        #[unsafe(method_id(supportedVolumeCapabilities))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn supported_volume_capabilities(&self) -> Retained<FSVolumeSupportedCapabilities> {
            unsafe {
                let caps = FSVolumeSupportedCapabilities::new();
                caps.setSupports64BitObjectIDs(true);
                caps.setDoesNotSupportVolumeSizes(true);
                caps.setDoesNotSupportSettingFilePermissions(true);
                caps.setCaseFormat(FSVolumeCaseFormat::Sensitive);
                caps
            }
        }
        #[unsafe(method_id(volumeStatistics))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn volume_statistics(&self) -> Retained<FSStatFSResult> {
            unsafe {
                let stats = FSStatFSResult::initWithFileSystemTypeName(
                    FSStatFSResult::alloc(),
                    &objc2_foundation::NSString::from_str("Polymer"),
                );
                stats.setBlockSize(4096);
                stats.setIoSize(MAX_IO as isize);
                stats
            }
        }
        #[unsafe(method(enableOpenUnlinkEmulation))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn enable_open_unlink_emulation(&self) -> bool {
            unsafe { true }
        }
        #[unsafe(method(mountWithOptions:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn mount_with_options_reply_handler(
            &self,
            options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            unsafe {
                done!(self, reply, self.call(Request::Sync));
            }
        }
        #[unsafe(method(unmountWithReplyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn unmount_with_reply_handler(&self, reply: &block2::DynBlock<dyn Fn()>) {
            unsafe {
                let _ = self.with_namespace(|| self.call(Request::Sync));
                reply.call(());
            }
        }
        #[unsafe(method(synchronizeWithFlags:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn synchronize_with_flags_reply_handler(
            &self,
            flags: FSSyncFlags,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            unsafe {
                done!(self, reply, self.call(Request::Sync));
            }
        }
        #[unsafe(method(getAttributes:ofItem:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn get_attributes_of_item_reply_handler(
            &self,
            desired_attributes: &FSItemGetAttributesRequest,
            item: Option<&FSItem>,
            reply: &block2::DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        ) {
            unsafe {
                // FSKit can request attributes for a missing rename target on
                // macOS 27, despite the nonnull annotation in FSVolume.h.
                object_reply!(
                    self,
                    reply,
                    item.ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
                        .and_then(|item| self.stat(item))
                );
            }
        }
        #[unsafe(method(setAttributes:onItem:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn set_attributes_on_item_reply_handler(
            &self,
            new_attributes: &FSItemSetAttributesRequest,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        ) {
            unsafe {
                object_reply!(self, reply, self.set_attributes(item, new_attributes));
            }
        }
        #[unsafe(method(lookupItemNamed:inDirectory:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn lookup_item_named_in_directory_reply_handler(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            unsafe {
                item_reply!(
                    self,
                    reply,
                    Self::child_path(directory, name).and_then(|path| self.item(path)),
                    name
                );
            }
        }
        #[unsafe(method(reclaimItem:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn reclaim_item_reply_handler(
            &self,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            unsafe {
                done!(self, reply, self.close(item));
            }
        }
        #[unsafe(method(readSymbolicLink:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn read_symbolic_link_reply_handler(
            &self,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            unsafe {
                reply.call((
                    ptr::null_mut(),
                    Retained::as_ptr(&Error::unsupported()).cast_mut(),
                ));
            }
        }
        #[unsafe(method(createItemNamed:type:inDirectory:attributes:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn create_item_named_type_in_directory_attributes_reply_handler(
            &self,
            name: &FSFileName,
            r#type: FSItemType,
            directory: &FSItem,
            new_attributes: &FSItemSetAttributesRequest,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            unsafe {
                item_reply!(
                    self,
                    reply,
                    self.create(directory, name, r#type, new_attributes),
                    name
                );
            }
        }
        #[unsafe(method(createSymbolicLinkNamed:inDirectory:attributes:linkContents:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn create_symbolic_link_named_in_directory_attributes_link_contents_reply_handler(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            new_attributes: &FSItemSetAttributesRequest,
            contents: &FSFileName,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            unsafe {
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::as_ptr(&Error::unsupported()).cast_mut(),
                ));
            }
        }
        #[unsafe(method(createLinkToItem:named:inDirectory:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn create_link_to_item_named_in_directory_reply_handler(
            &self,
            item: &FSItem,
            name: &FSFileName,
            directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            unsafe {
                reply.call((
                    ptr::null_mut(),
                    Retained::as_ptr(&Error::unsupported()).cast_mut(),
                ));
            }
        }
        #[unsafe(method(removeItem:named:fromDirectory:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn remove_item_named_from_directory_reply_handler(
            &self,
            item: &FSItem,
            name: &FSFileName,
            directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            unsafe {
                done!(self, reply, self.remove(item, directory, name));
            }
        }
        #[unsafe(method(renameItem:inDirectory:named:toNewName:inDirectory:overItem:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn rename_item_in_directory_named_to_new_name_in_directory_over_item_reply_handler(
            &self,
            item: &FSItem,
            source_directory: &FSItem,
            source_name: &FSFileName,
            destination_name: &FSFileName,
            destination_directory: &FSItem,
            over_item: Option<&FSItem>,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            unsafe {
                match self.with_namespace(|| {
                    self.rename(
                        item,
                        destination_directory,
                        destination_name,
                        over_item.is_some(),
                    )
                }) {
                    Ok(()) => reply.call((
                        destination_name as *const FSFileName as *mut FSFileName,
                        ptr::null_mut(),
                    )),
                    Err(error) => reply.call((
                        ptr::null_mut(),
                        Retained::as_ptr(&Error::native(error)).cast_mut(),
                    )),
                }
            }
        }
        #[unsafe(method(enumerateDirectory:startingAtCookie:verifier:providingAttributes:usingPacker:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn enumerate_directory_starting_at_cookie_verifier_providing_attributes_using_packer_reply_handler(
            &self,
            directory: &FSItem,
            cookie: FSDirectoryCookie,
            verifier: FSDirectoryVerifier,
            attributes: Option<&FSItemGetAttributesRequest>,
            packer: &FSDirectoryEntryPacker,
            reply: &block2::DynBlock<dyn Fn(FSDirectoryVerifier, *mut NSError)>,
        ) {
            unsafe {
                match self.with_namespace(|| {
                    self.enumerate(directory, cookie, verifier, attributes.is_some(), packer)
                }) {
                    Ok(verifier) => reply.call((verifier, ptr::null_mut())),
                    Err(error) => {
                        reply.call((0, Retained::as_ptr(&Error::native(error)).cast_mut()))
                    }
                }
            }
        }
        #[unsafe(method(activateWithOptions:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn activate_with_options_reply_handler(
            &self,
            options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut NSError)>,
        ) {
            unsafe {
                object_reply!(self, reply, self.item(b"/".to_vec()));
            }
        }
        #[unsafe(method(deactivateWithOptions:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn deactivate_with_options_reply_handler(
            &self,
            options: FSDeactivateOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            unsafe {
                self.with_namespace(|| self.ivars().items.clear());
                reply.call((ptr::null_mut(),));
            }
        }
    }
    unsafe impl FSVolumeOpenCloseOperations for Volume {
        #[unsafe(method(openItem:withModes:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn open_item_with_modes_reply_handler(
            &self,
            item: &FSItem,
            modes: FSVolumeOpenModes,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            unsafe {
                done!(self, reply, self.open(item, modes));
            }
        }
        #[unsafe(method(closeItem:keepingModes:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn close_item_keeping_modes_reply_handler(
            &self,
            item: &FSItem,
            modes: FSVolumeOpenModes,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            unsafe {
                if modes.is_empty() {
                    done!(self, reply, self.close(item));
                } else {
                    reply.call((ptr::null_mut(),));
                }
            }
        }
    }
    unsafe impl FSVolumeReadWriteOperations for Volume {
        #[unsafe(method(readFromFile:offset:length:intoBuffer:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn read_from_file_offset_length_into_buffer_reply_handler(
            &self,
            item: &FSItem,
            offset: libc::off_t,
            length: usize,
            buffer: &FSMutableFileDataBuffer,
            reply: &block2::DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            unsafe {
                match self.with_namespace(|| self.read(item, offset, length, buffer)) {
                    Ok(count) => reply.call((count, ptr::null_mut())),
                    Err(error) => {
                        reply.call((0, Retained::as_ptr(&Error::native(error)).cast_mut()))
                    }
                }
            }
        }
        #[unsafe(method(writeContents:toFile:atOffset:replyHandler:))]
        #[allow(unused_variables, unused_unsafe)]
        unsafe fn write_contents_to_file_at_offset_reply_handler(
            &self,
            contents: &NSData,
            item: &FSItem,
            offset: libc::off_t,
            reply: &block2::DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            unsafe {
                match self.with_namespace(|| self.write(item, offset, contents)) {
                    Ok(count) => reply.call((count, ptr::null_mut())),
                    Err(error) => {
                        reply.call((0, Retained::as_ptr(&Error::native(error)).cast_mut()))
                    }
                }
            }
        }
    }
);
