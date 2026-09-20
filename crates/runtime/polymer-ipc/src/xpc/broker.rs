use super::{BROKER_SERVICE, Interface, StatusReply, TIMEOUT};
use block2::{ManualBlockEncoding, RcBlock};
use dashmap::DashMap;
use objc2::{
    AnyThread, DefinedClass, Message as _, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject,
};
use objc2_foundation::{
    NSError, NSObject, NSObjectProtocol, NSString, NSXPCConnection, NSXPCConnectionOptions,
    NSXPCListener, NSXPCListenerDelegate, NSXPCListenerEndpoint,
};
use std::{
    ffi::CStr,
    io,
    ptr::{self, NonNull},
    sync::{Arc, Mutex, mpsc},
};

pub(super) struct Directory(Retained<NSXPCConnection>);
// SAFETY: NSXPCConnection can be called and invalidated from any thread.
unsafe impl Send for Directory {}
unsafe impl Sync for Directory {}
impl Drop for Directory {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}
impl Directory {
    fn connect() -> Self {
        let native = NSXPCConnection::initWithMachServiceName_options(
            NSXPCConnection::alloc(),
            &NSString::from_str(BROKER_SERVICE),
            NSXPCConnectionOptions::empty(),
        );
        native.setRemoteObjectInterface(Some(&Interface::directory()));
        native.resume();
        Self(native)
    }
    pub fn publish(name: &str, endpoint: &NSXPCListenerEndpoint) -> io::Result<Self> {
        let directory = Self::connect();
        let (sender, receiver) = mpsc::sync_channel(1);
        let failure = sender.clone();
        let error = RcBlock::new(move |error: NonNull<NSError>| {
            let _ = failure.try_send(Err(io::Error::other(unsafe { error.as_ref() }.to_string())));
        });
        let reply = RcBlock::with_encoding::<_, _, _, StatusReply>(move |status: i32| {
            let _ = sender.try_send(Interface::status(status));
        });
        let proxy = directory.0.remoteObjectProxyWithErrorHandler(&error);
        unsafe {
            polymer_publish(&proxy, &NSString::from_str(name), endpoint, &reply);
        }
        receiver.recv_timeout(TIMEOUT).map_err(io::Error::other)??;
        Ok(directory)
    }
    pub fn resolve(name: &str) -> io::Result<Retained<NSXPCListenerEndpoint>> {
        let directory = Self::connect();
        let (sender, receiver) = mpsc::sync_channel(1);
        let failure = sender.clone();
        let error = RcBlock::new(move |error: NonNull<NSError>| {
            let _ = failure.try_send(Err(io::Error::other(unsafe { error.as_ref() }.to_string())));
        });
        let reply = RcBlock::with_encoding::<_, _, _, EndpointReply>(
            move |endpoint: *mut NSXPCListenerEndpoint| {
                let result = unsafe { Retained::retain(endpoint) }
                    .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound));
                let _ = sender.try_send(result);
            },
        );
        let proxy = directory.0.remoteObjectProxyWithErrorHandler(&error);
        unsafe {
            polymer_resolve(&proxy, &NSString::from_str(name), &reply);
        }
        receiver.recv_timeout(TIMEOUT).map_err(io::Error::other)?
    }
}
struct EndpointReply;
// SAFETY: void (^)(NSXPCListenerEndpoint *), with NSXPC's required class metadata.
unsafe impl ManualBlockEncoding for EndpointReply {
    type Arguments = (*mut NSXPCListenerEndpoint,);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = cr#"v16@?0@"NSXPCListenerEndpoint"8"#;
}

type Registry = Arc<DashMap<String, Retained<NSXPCListenerEndpoint>>>;
struct Registration {
    registry: Registry,
    name: Mutex<Option<String>>,
}
impl Registration {
    fn close(&self) {
        if let Some(name) = self.name.lock().unwrap().take() {
            self.registry.remove(&name);
        }
    }
}
define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Arc<Registration>]
    struct DirectorySession;
    unsafe impl NSObjectProtocol for DirectorySession {}
    impl DirectorySession {
        #[unsafe(method(publish:endpoint:reply:))]
        fn publish(&self, name: &NSString, endpoint: &NSXPCListenerEndpoint, reply: &block2::DynBlock<dyn Fn(i32)>) {
            let status = self.register(name.to_string(), endpoint);
            reply.call((status,));
        }
        #[unsafe(method(resolve:reply:))]
        fn resolve(&self, name: &NSString, reply: &block2::DynBlock<dyn Fn(*mut NSXPCListenerEndpoint)>) {
            let endpoint = self.ivars().registry.get(&name.to_string()).map(|entry| entry.clone());
            reply.call((endpoint.as_ref().map_or(ptr::null_mut(), |value| Retained::as_ptr(value).cast_mut()),));
        }
    }
);
impl DirectorySession {
    fn register(&self, name: String, endpoint: &NSXPCListenerEndpoint) -> i32 {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return libc::EINVAL;
        }
        let mut owned = self.ivars().name.lock().unwrap();
        if owned.is_some() {
            return libc::EALREADY;
        }
        match self.ivars().registry.entry(name.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => libc::EADDRINUSE,
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(endpoint.retain());
                *owned = Some(name);
                0
            }
        }
    }
}
define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Registry]
    struct BrokerDelegate;
    unsafe impl NSObjectProtocol for BrokerDelegate {}
    unsafe impl NSXPCListenerDelegate for BrokerDelegate {
        #[unsafe(method(listener:shouldAcceptNewConnection:))]
        fn accept(&self, _: &NSXPCListener, native: &NSXPCConnection) -> bool {
            if native.effectiveUserIdentifier() != unsafe { libc::geteuid() } {
                return false.into();
            }
            let registration = Arc::new(Registration {
                registry: self.ivars().clone(),
                name: Mutex::new(None),
            });
            let object = DirectorySession::alloc().set_ivars(registration.clone());
            let object: Retained<DirectorySession> = unsafe { msg_send![super(object), init] };
            native.setExportedInterface(Some(&Interface::directory()));
            unsafe {
                native.setExportedObject(Some(&object));
            }
            let handler = RcBlock::new(move || registration.close());
            native.setInvalidationHandler(Some(&handler));
            native.setInterruptionHandler(Some(&handler));
            native.resume();
            true
        }
    }
);

/// Per-user launchd service. It transfers listener capabilities, never payloads.
pub struct Broker {
    listener: Retained<NSXPCListener>,
    _delegate: Retained<BrokerDelegate>,
}
impl Broker {
    pub fn start() -> Self {
        let delegate = BrokerDelegate::alloc().set_ivars(Arc::new(DashMap::new()));
        let delegate: Retained<BrokerDelegate> = unsafe { msg_send![super(delegate), init] };
        let listener = NSXPCListener::initWithMachServiceName(
            NSXPCListener::alloc(),
            &NSString::from_str(BROKER_SERVICE),
        );
        listener.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        listener.resume();
        Self {
            listener,
            _delegate: delegate,
        }
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.listener.invalidate();
    }
}

unsafe extern "C" {
    fn polymer_publish(
        proxy: &objc2::runtime::AnyObject,
        name: &NSString,
        endpoint: &NSXPCListenerEndpoint,
        reply: &block2::DynBlock<dyn Fn(i32)>,
    );
    fn polymer_resolve(
        proxy: &objc2::runtime::AnyObject,
        name: &NSString,
        reply: &block2::DynBlock<dyn Fn(*mut NSXPCListenerEndpoint)>,
    );
}
