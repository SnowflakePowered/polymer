use crate::wire::{Attributes, Entry};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, rc::Retained};
use objc2_foundation::NSObjectProtocol;
use objc2_fs_kit::{FSItem, FSItemAttributes, FSItemID, FSItemType};
use parking_lot::{Mutex, RwLock};

pub(super) struct ItemState {
    pub id: u64,
    pub path: RwLock<Vec<u8>>,
    pub handle: Mutex<Option<(u64, bool, bool)>>,
    pub entries: Mutex<DirectorySnapshot>,
}

#[derive(Default)]
pub(super) struct DirectorySnapshot {
    pub verifier: u64,
    pub attributes: bool,
    pub entries: Vec<Entry>,
}

define_class!(
    #[unsafe(super(FSItem))]
    #[ivars = ItemState]
    pub(super) struct Item;
    unsafe impl NSObjectProtocol for Item {}
);
impl Item {
    pub fn new(id: u64, path: Vec<u8>) -> Retained<Self> {
        let item = Self::alloc().set_ivars(ItemState {
            id,
            path: RwLock::new(path),
            handle: Mutex::new(None),
            entries: Mutex::new(DirectorySnapshot::default()),
        });
        unsafe { msg_send![super(item), init] }
    }
    pub fn path(&self) -> Vec<u8> {
        self.ivars().path.read().clone()
    }
    pub fn attributes(&self, value: &Attributes) -> Retained<FSItemAttributes> {
        unsafe {
            let result = FSItemAttributes::new();
            result.setType(if value.directory {
                FSItemType::Directory
            } else {
                FSItemType::File
            });
            result.setMode(value.mode);
            result.setSize(value.size);
            result.setAllocSize(value.size.div_ceil(4096) * 4096);
            result.setFileID(FSItemID(self.ivars().id));
            result.setLinkCount(1);
            result.setUid(libc::getuid());
            result.setGid(libc::getgid());
            result.setInhibitKernelOffloadedIO(true);
            result
        }
    }
}
