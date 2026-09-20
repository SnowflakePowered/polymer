use super::{Connection, broker::Directory, queue::Queue};
use objc2::{
    AnyThread, DefinedClass, Message as _, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject,
};
use objc2_foundation::{
    NSObject, NSObjectProtocol, NSXPCConnection, NSXPCListener, NSXPCListenerDelegate,
};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Arc<Queue<Connection>>]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSXPCListenerDelegate for Delegate {
        #[unsafe(method(listener:shouldAcceptNewConnection:))]
        fn accept(&self, _: &NSXPCListener, native: &NSXPCConnection) -> bool {
            if native.effectiveUserIdentifier() != unsafe { libc::geteuid() } { return false.into(); }
            let connection = Connection::attach(native.retain());
            self.ivars().push(connection)
        }
    }
);
#[derive(Clone)]
pub struct Listener(Arc<Inner>);
struct Inner {
    native: Retained<NSXPCListener>,
    _delegate: Retained<Delegate>,
    directory: Mutex<Option<Directory>>,
    incoming: Arc<Queue<Connection>>,
}
// SAFETY: NSXPCListener supports thread-safe invalidation. Delegate state and
// the directory registration are synchronized; accepted connections own state.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}
impl Inner {
    fn close(&self) {
        self.native.invalidate();
        self.incoming.discard();
        self.directory.lock().unwrap().take();
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}
impl Listener {
    pub fn bind(name: &str) -> io::Result<Self> {
        let incoming = Arc::new(Queue::default());
        let delegate = Delegate::alloc().set_ivars(incoming.clone());
        let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
        let native = NSXPCListener::anonymousListener();
        native.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        native.resume();
        let directory = match Directory::publish(name, &native.endpoint()) {
            Ok(directory) => directory,
            Err(error) => {
                native.invalidate();
                return Err(error);
            }
        };
        Ok(Self(Arc::new(Inner {
            native,
            _delegate: delegate,
            directory: Mutex::new(Some(directory)),
            incoming,
        })))
    }
    pub fn accept(&self) -> io::Result<Connection> {
        self.0.incoming.receive(None)
    }
    pub fn accept_timeout(&self, timeout: Duration) -> io::Result<Connection> {
        self.0.incoming.receive(Some(timeout))
    }
    pub fn close(&self) {
        self.0.close();
    }
}
