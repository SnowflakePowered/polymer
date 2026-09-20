use block2::RcBlock;
use dashmap::DashMap;
use objc2::{
    Message,
    rc::Retained,
    runtime::{Imp, ProtocolObject, Sel},
    sel,
};
use objc2_metal::*;
use polymer_producer_metal::swapchain::PresentWork;
use std::{
    io,
    ptr::NonNull,
    sync::{LazyLock, Mutex},
};

type Commit = unsafe extern "C-unwind" fn(&ProtocolObject<dyn MTLCommandBuffer>, Sel);
struct Pending(Retained<ProtocolObject<dyn MTLCommandBuffer>>);
// SAFETY: Metal command buffers permit status queries and waits from other threads.
unsafe impl Send for Pending {}
unsafe impl Sync for Pending {}
static PENDING: LazyLock<DashMap<usize, Pending>> = LazyLock::new(DashMap::new);
static ORIGINALS: LazyLock<DashMap<usize, Commit>> = LazyLock::new(DashMap::new);
static INSTALL: Mutex<()> = Mutex::new(());
pub(super) struct Tracker;
impl Tracker {
    pub fn install(device: &ProtocolObject<dyn MTLDevice>) -> io::Result<()> {
        let queue = device
            .newCommandQueue()
            .ok_or_else(|| io::Error::other("Metal queue unavailable"))?;
        let buffer = queue
            .commandBuffer()
            .ok_or_else(|| io::Error::other("Metal command buffer unavailable"))?;
        let _guard = INSTALL.lock().unwrap();
        let object: &objc2::runtime::AnyObject = buffer.as_ref();
        let class = object.class();
        let key = class as *const _ as usize;
        if ORIGINALS.contains_key(&key) {
            return Ok(());
        }
        let method = class
            .instance_method(sel!(commit))
            .ok_or_else(|| io::Error::other("Metal commit selector missing"))?;
        // SAFETY: commit has the exact Objective-C receiver/selector ABI.
        unsafe {
            let original: Commit = std::mem::transmute(method.implementation());
            if original as usize == Self::commit as *const () as usize {
                return Ok(());
            }
            ORIGINALS.insert(key, original);
            method.set_implementation(std::mem::transmute::<Commit, Imp>(Self::commit));
        }
        Ok(())
    }
    unsafe extern "C-unwind" fn commit(
        buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        selector: Sel,
    ) {
        let key = buffer as *const _ as usize;
        PENDING.insert(key, Pending(buffer.retain()));
        let done = RcBlock::new(
            move |buffer: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                if unsafe { buffer.as_ref() }.status() == MTLCommandBufferStatus::Completed {
                    PENDING.remove(&key);
                }
            },
        );
        unsafe {
            buffer.addCompletedHandler(RcBlock::as_ptr(&done));
        }
        let object: &objc2::runtime::AnyObject = buffer.as_ref();
        let mut class = Some(object.class());
        let original = loop {
            let current = class.expect("installed Metal commit class");
            if let Some(original) = ORIGINALS.get(&(current as *const _ as usize)) {
                break *original;
            }
            class = current.superclass();
        };
        unsafe {
            original(buffer, selector);
        }
    }
    pub fn submitted() -> PresentWork {
        PresentWork(PENDING.iter().map(|entry| entry.0.clone()).collect())
    }
}
