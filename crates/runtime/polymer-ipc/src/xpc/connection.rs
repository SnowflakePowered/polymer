use super::{Interface, MAX_PACKET_SIZE, StatusReply, TIMEOUT, broker::Directory, queue::Queue};
use block2::{ManualBlockEncoding, RcBlock};
use objc2::{AnyThread, DefinedClass, Message as _, define_class, msg_send, rc::Retained};
use objc2_foundation::{
    NSArray, NSData, NSError, NSFileHandle, NSObject, NSObjectProtocol, NSXPCConnection,
};
use objc2_io_surface::IOSurface;
use objc2_metal::MTLSharedEventHandle;
use std::{
    ffi::CStr,
    fs::File,
    io,
    os::fd::{FromRawFd, IntoRawFd},
    ptr::{self, NonNull},
    sync::{Arc, mpsc},
    time::Duration,
};

#[derive(Default)]
pub struct Packet {
    pub data: Vec<u8>,
    pub file: Option<File>,
    pub surfaces: Vec<Retained<IOSurface>>,
    pub event: Option<Retained<MTLSharedEventHandle>>,
    pub consumer_event: Option<Retained<MTLSharedEventHandle>>,
}
// SAFETY: IOSurface and shared-event handles support use across threads. Packet
// owns immutable retained references and transfers file ownership with File.
unsafe impl Send for Packet {}

/// A bytes-only request awaiting exactly one reply. Dropping it unanswered
/// replies with `ECANCELED`.
pub struct Call {
    pub data: Vec<u8>,
    reply: Option<RcBlock<dyn Fn(*mut NSData, i32) + 'static>>,
}
// SAFETY: NSXPC reply blocks may be invoked once from any thread.
unsafe impl Send for Call {}
impl Call {
    pub fn reply(mut self, data: &[u8]) {
        let Some(reply) = self.reply.take() else {
            return;
        };
        if data.len() > MAX_PACKET_SIZE {
            reply.call((ptr::null_mut(), libc::EMSGSIZE));
            return;
        }
        let data = NSData::with_bytes(data);
        reply.call((Retained::as_ptr(&data).cast_mut(), 0));
    }
}
impl Drop for Call {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            reply.call((ptr::null_mut(), libc::ECANCELED));
        }
    }
}

struct DataReply;
// SAFETY: void (^)(NSData *, int), with NSXPC's required class metadata.
unsafe impl ManualBlockEncoding for DataReply {
    type Arguments = (*mut NSData, i32);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = cr#"v20@?0@"NSData"8i16"#;
}

#[derive(Default)]
struct Incoming {
    packets: Queue<Packet>,
    calls: Queue<Call>,
}
impl Incoming {
    fn close(&self) {
        self.packets.close();
        self.calls.close();
    }
}

#[derive(Clone)]
pub struct Connection(Arc<Inner>);
struct Inner {
    native: Retained<NSXPCConnection>,
    incoming: Arc<Incoming>,
}
// SAFETY: NSXPCConnection supports concurrent proxy calls; incoming packets are
// serialized by Queue. Native callbacks never borrow the Connection's caller.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}
impl Drop for Inner {
    fn drop(&mut self) {
        self.native.invalidate();
        self.incoming.close();
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Arc<Incoming>]
    struct Receiver;
    unsafe impl NSObjectProtocol for Receiver {}
    impl Receiver {
        #[unsafe(method(ping:))]
        fn ping(&self, reply: &block2::DynBlock<dyn Fn(i32)>) { reply.call((0,)); }
        #[unsafe(method(sendData:file:surfaces:event:consumerEvent:reply:))]
        fn receive(&self, data: Option<&NSData>, file: Option<&NSFileHandle>,
            surfaces: Option<&NSArray<IOSurface>>, event: Option<&MTLSharedEventHandle>,
            consumer_event: Option<&MTLSharedEventHandle>, reply: &block2::DynBlock<dyn Fn(i32)>) {
            let (Some(data), Some(surfaces)) = (data, surfaces) else {
                reply.call((libc::EINVAL,));
                return;
            };
            let status = match self.packet(data, file, surfaces, event, consumer_event) {
                Ok(packet) => if self.ivars().packets.push(packet) { 0 } else { libc::ENOBUFS },
                Err(error) => error.raw_os_error().unwrap_or(libc::EINVAL),
            };
            reply.call((status,));
        }
        #[unsafe(method(call:reply:))]
        fn call(&self, data: Option<&NSData>, reply: &block2::DynBlock<dyn Fn(*mut NSData, i32) + 'static>) {
            let Some(data) = data.filter(|data| data.len() <= MAX_PACKET_SIZE) else {
                reply.call((ptr::null_mut(), libc::EMSGSIZE));
                return;
            };
            self.ivars().calls.push(Call { data: data.to_vec(), reply: Some(reply.copy()) });
        }
    }
);
impl Receiver {
    fn packet(
        &self,
        data: &NSData,
        file: Option<&NSFileHandle>,
        surfaces: &NSArray<IOSurface>,
        event: Option<&MTLSharedEventHandle>,
        consumer_event: Option<&MTLSharedEventHandle>,
    ) -> io::Result<Packet> {
        if data.len() > MAX_PACKET_SIZE || surfaces.len() > polymer_protocol::MAX_IMAGE_SLOTS {
            return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
        }
        let file = file
            .map(|file| {
                // NSFileHandle owns the received descriptor. Duplicate before its
                // autorelease scope ends; never import an integer from message data.
                let fd = unsafe { libc::fcntl(file.fileDescriptor(), libc::F_DUPFD_CLOEXEC, 0) };
                if fd < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(unsafe { File::from_raw_fd(fd) })
                }
            })
            .transpose()?;
        Ok(Packet {
            data: data.to_vec(),
            file,
            surfaces: surfaces.iter().collect(),
            event: event.map(|value| value.retain()),
            consumer_event: consumer_event.map(|value| value.retain()),
        })
    }
}
impl Connection {
    pub fn connect(name: &str) -> io::Result<Self> {
        let endpoint = Directory::resolve(name)?;
        let connection = Self::attach(NSXPCConnection::initWithListenerEndpoint(
            NSXPCConnection::alloc(),
            &endpoint,
        ));
        connection.activate()?;
        Ok(connection)
    }
    fn activate(&self) -> io::Result<()> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let failure = sender.clone();
        let error = RcBlock::new(move |error: NonNull<NSError>| {
            let _ = failure.try_send(Err(io::Error::other(unsafe { error.as_ref() }.to_string())));
        });
        let reply = RcBlock::with_encoding::<_, _, _, StatusReply>(move |status: i32| {
            let _ = sender.try_send(Interface::status(status));
        });
        let proxy = self.0.native.remoteObjectProxyWithErrorHandler(&error);
        unsafe {
            polymer_ping(&proxy, &reply);
        }
        receiver.recv_timeout(TIMEOUT).map_err(io::Error::other)?
    }
    pub(super) fn attach(native: Retained<NSXPCConnection>) -> Self {
        let incoming = Arc::new(Incoming::default());
        let receiver = Receiver::alloc().set_ivars(incoming.clone());
        let receiver: Retained<Receiver> = unsafe { msg_send![super(receiver), init] };
        native.setExportedInterface(Some(&Interface::packet()));
        native.setRemoteObjectInterface(Some(&Interface::packet()));
        unsafe {
            native.setExportedObject(Some(&receiver));
        }
        let closed = incoming.clone();
        let handler = RcBlock::new(move || closed.close());
        native.setInvalidationHandler(Some(&handler));
        native.setInterruptionHandler(Some(&handler));
        native.resume();
        Self(Arc::new(Inner { native, incoming }))
    }
    pub fn peer_process_id(&self) -> u32 {
        self.0.native.processIdentifier() as u32
    }
    pub fn send(&self, packet: Packet, timeout: Option<Duration>) -> io::Result<()> {
        if packet.data.len() > MAX_PACKET_SIZE
            || packet.surfaces.len() > polymer_protocol::MAX_IMAGE_SLOTS
        {
            return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
        }
        let file = packet.file.map(|file| {
            NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                file.into_raw_fd(),
                true,
            )
        });
        let surfaces = NSArray::from_retained_slice(&packet.surfaces);
        let (sender, receiver) = mpsc::sync_channel(1);
        let failure = sender.clone();
        let error = RcBlock::new(move |error: NonNull<NSError>| {
            let _ = failure.try_send(Err(io::Error::other(unsafe { error.as_ref() }.to_string())));
        });
        let reply = RcBlock::with_encoding::<_, _, _, StatusReply>(move |status: i32| {
            let _ = sender.try_send(Interface::status(status));
        });
        let proxy = self.0.native.remoteObjectProxyWithErrorHandler(&error);
        unsafe {
            polymer_send(
                &proxy,
                &NSData::with_bytes(&packet.data),
                file.as_deref().map_or(std::ptr::null(), |v| v),
                &surfaces,
                packet.event.as_deref().map_or(std::ptr::null(), |v| v),
                packet
                    .consumer_event
                    .as_deref()
                    .map_or(std::ptr::null(), |v| v),
                &reply,
            );
        }
        match receiver.recv_timeout(timeout.unwrap_or(TIMEOUT)) {
            Ok(result) => result,
            Err(error) => {
                // Delivery is uncertain after a timeout. Prevent a late reply
                // from being mistaken for a later application request.
                self.close();
                Err(io::Error::new(
                    match error {
                        mpsc::RecvTimeoutError::Timeout => io::ErrorKind::TimedOut,
                        mpsc::RecvTimeoutError::Disconnected => io::ErrorKind::BrokenPipe,
                    },
                    error,
                ))
            }
        }
    }
    pub fn receive(&self, timeout: Option<Duration>) -> io::Result<Packet> {
        self.0.incoming.packets.receive(timeout)
    }
    pub fn call(&self, data: &[u8], timeout: Option<Duration>) -> io::Result<Vec<u8>> {
        if data.len() > MAX_PACKET_SIZE {
            return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let failure = sender.clone();
        let error = RcBlock::new(move |error: NonNull<NSError>| {
            let _ = failure.try_send(Err(io::Error::other(unsafe { error.as_ref() }.to_string())));
        });
        let reply =
            RcBlock::with_encoding::<_, _, _, DataReply>(move |data: *mut NSData, status: i32| {
                let result = Interface::status(status).and_then(|()| {
                    unsafe { data.as_ref() }
                        .filter(|data| data.len() <= MAX_PACKET_SIZE)
                        .map(|data| data.to_vec())
                        .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADMSG))
                });
                let _ = sender.try_send(result);
            });
        let proxy = self.0.native.remoteObjectProxyWithErrorHandler(&error);
        unsafe {
            polymer_call(&proxy, &NSData::with_bytes(data), &reply);
        }
        match receiver.recv_timeout(timeout.unwrap_or(TIMEOUT)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(io::ErrorKind::TimedOut.into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
    pub fn receive_call(&self, timeout: Option<Duration>) -> io::Result<Call> {
        self.0.incoming.calls.receive(timeout)
    }
    pub fn close(&self) {
        self.0.native.invalidate();
        self.0.incoming.close();
    }
    pub fn peer_disconnected(&self) -> bool {
        self.0.incoming.packets.closed()
    }
}

unsafe extern "C" {
    fn polymer_call(
        proxy: &objc2::runtime::AnyObject,
        data: &NSData,
        reply: &block2::DynBlock<dyn Fn(*mut NSData, i32)>,
    );
    fn polymer_ping(proxy: &objc2::runtime::AnyObject, reply: &block2::DynBlock<dyn Fn(i32)>);
    fn polymer_send(
        proxy: &objc2::runtime::AnyObject,
        data: &NSData,
        file: *const NSFileHandle,
        surfaces: &NSArray<IOSurface>,
        event: *const MTLSharedEventHandle,
        consumer_event: *const MTLSharedEventHandle,
        reply: &block2::DynBlock<dyn Fn(i32)>,
    );
}
