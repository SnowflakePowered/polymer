//! Native macOS messaging and capability transfer. Shared rings remain the data path.
mod broker;
mod connection;
mod listener;
mod queue;

use block2::ManualBlockEncoding;
pub use broker::Broker;
pub use connection::{Call, Connection, Packet};
pub use listener::Listener;
use objc2::{ClassType, rc::Retained, runtime::AnyProtocol, sel};
use objc2_foundation::{NSArray, NSSet, NSXPCInterface};
use objc2_io_surface::IOSurface;
use std::{ffi::CStr, io, time::Duration};

pub const BROKER_SERVICE: &str = "gg.polymer.ipc";
pub const MAX_PACKET_SIZE: usize = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

unsafe extern "C" {
    fn polymer_packet_protocol() -> *const AnyProtocol;
    fn polymer_directory_protocol() -> *const AnyProtocol;
}

struct Interface;
impl Interface {
    fn packet() -> Retained<NSXPCInterface> {
        unsafe {
            let interface = NSXPCInterface::interfaceWithProtocol(&*polymer_packet_protocol());
            let classes = NSSet::from_slice(&[IOSurface::class(), NSArray::<IOSurface>::class()]);
            interface.setClasses_forSelector_argumentIndex_ofReply(
                &classes,
                sel!(sendData:file:surfaces:event:consumerEvent:reply:),
                2,
                false,
            );
            interface
        }
    }
    fn directory() -> Retained<NSXPCInterface> {
        unsafe { NSXPCInterface::interfaceWithProtocol(&*polymer_directory_protocol()) }
    }
    fn status(code: i32) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code))
        }
    }
}

struct StatusReply;
// SAFETY: void (^)(int), including the implicit block pointer on 64-bit macOS.
unsafe impl ManualBlockEncoding for StatusReply {
    type Arguments = (i32,);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v12@?0i8";
}

/// Native objects retained for one immutable Metal stream generation.
#[derive(Clone, Debug)]
pub struct MetalResources {
    pub surfaces: Vec<Retained<IOSurface>>,
    pub ready: Retained<objc2_metal::MTLSharedEventHandle>,
    pub released: Retained<objc2_metal::MTLSharedEventHandle>,
}
// SAFETY: these are immutable, retained cross-process resource capabilities.
// GPU access to the surfaces is synchronized separately by the shared events.
unsafe impl Send for MetalResources {}
unsafe impl Sync for MetalResources {}
