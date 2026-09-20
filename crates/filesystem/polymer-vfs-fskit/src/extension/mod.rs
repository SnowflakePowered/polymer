//! Rust FSKit classes. The extension's Swift entry point only supplies lifecycle.
mod connection;
mod item;
mod volume;

use connection::{Error, Resource};
use objc2::{AnyThread, define_class, msg_send, rc::Retained};
use objc2_foundation::{NSError, NSObjectProtocol, NSString};
use objc2_fs_kit::*;
use std::ptr;
use volume::Volume;

define_class!(
    #[unsafe(super(FSUnaryFileSystem))]
    #[name = "PolymerFileSystem"]
    pub struct PolymerFileSystem;
    unsafe impl NSObjectProtocol for PolymerFileSystem {}
    unsafe impl FSUnaryFileSystemOperations for PolymerFileSystem {
        #[unsafe(method(probeResource:replyHandler:))]
        unsafe fn probe(
            &self,
            resource: &FSResource,
            reply: &block2::DynBlock<dyn Fn(*mut FSProbeResult, *mut NSError)>,
        ) {
            let result = Resource::parse(resource);
            let probe = unsafe {
                match result {
                    Ok(resource) => {
                        let id = FSContainerIdentifier::initWithUUID(
                            FSContainerIdentifier::alloc(),
                            &resource.identifier().uuid(),
                        );
                        FSProbeResult::usableProbeResultWithName_containerID(
                            &NSString::from_str(&resource.name),
                            &id,
                        )
                    }
                    Err(_) => FSProbeResult::notRecognizedProbeResult(),
                }
            };
            reply.call((Retained::as_ptr(&probe).cast_mut(), ptr::null_mut()));
        }
        #[unsafe(method(loadResource:options:replyHandler:))]
        unsafe fn load(
            &self,
            resource: &FSResource,
            options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut FSVolume, *mut NSError)>,
        ) {
            let options = unsafe { options.taskOptions() };
            let result = if options.iter().any(|value| value.to_string() == "-f") {
                Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
            } else {
                Resource::parse(resource).and_then(|resource| {
                    Volume::new(
                        &resource,
                        options.iter().any(|value| value.to_string() == "--rdonly"),
                    )
                })
            };
            match result {
                Ok(volume) => {
                    unsafe { self.setContainerStatus(&FSContainerStatus::ready()) };
                    reply.call((Retained::as_ptr(&volume).cast_mut().cast(), ptr::null_mut()));
                }
                Err(error) => reply.call((
                    ptr::null_mut(),
                    Retained::as_ptr(&Error::native(error)).cast_mut(),
                )),
            }
        }
        #[unsafe(method(unloadResource:options:replyHandler:))]
        unsafe fn unload(
            &self,
            _: &FSResource,
            _: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            reply.call((ptr::null_mut(),));
        }
    }
);
impl PolymerFileSystem {
    /// Called only by the FSKit app extension bootstrap. Returns a +1 object.
    #[unsafe(no_mangle)]
    pub extern "C" fn PolymerCreateFileSystem() -> *mut PolymerFileSystem {
        let object: Retained<Self> = unsafe { msg_send![Self::alloc(), init] };
        Retained::into_raw(object)
    }
}
