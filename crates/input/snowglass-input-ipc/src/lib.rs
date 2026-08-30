mod control;
mod hid;
mod publisher;
mod topology;

pub use hid::{RawHidChannel, RawHidSpec};

use control::Control;
use parking_lot::Mutex;
use publisher::InputPublication;
use snowglass_input::{
    HostInputEvent, HostInputListener, HostListenerHandle, InputEvent, InputHub, InputListener,
    ListenerHandle,
};
use snowglass_input_layout::topology::InputTopology;
use snowglass_ipc::LocalSocketListener;
use std::collections::BTreeMap;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

#[derive(Clone)]
pub struct InputIpcListener(Arc<Inner>);

struct Inner {
    publication: Mutex<InputPublication>,
    control: Control,
    listener: LocalSocketListener,
    hub: Arc<Mutex<Option<ListenerHandle>>>,
    host: Mutex<Option<HostListenerHandle>>,
    stopping: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    raw_hid: Mutex<BTreeMap<snowglass_input::PlayerIndex, RawHidChannel>>,
}

impl InputIpcListener {
    pub fn bind(session: &str) -> io::Result<Self> {
        let listener = LocalSocketListener::bind(&format!("{session}-input"))?;
        let value = Self(Arc::new(Inner {
            publication: Mutex::new(InputPublication::new()?),
            control: Control::default(),
            listener,
            hub: Arc::new(Mutex::new(None)),
            host: Mutex::new(None),
            stopping: AtomicBool::new(false),
            worker: Mutex::new(None),
            raw_hid: Mutex::new(BTreeMap::new()),
        }));
        let worker = value.0.control.spawn_acceptor(value.clone())?;
        *value.0.worker.lock() = Some(worker);
        Ok(value)
    }

    pub fn attach(&self, hub: &InputHub) {
        let handle = hub.add_listener(Arc::new(self.clone()));
        hub.set_feedback_priority(Some(handle.id()));
        *self.0.hub.lock() = Some(handle);
        *self.0.host.lock() = Some(hub.add_host_listener(Arc::new(self.clone())));
    }

    pub fn publish_topology(&self, topology: &InputTopology) -> io::Result<()> {
        self.0.control.publish_topology(topology)
    }

    pub fn publish_host(&self, event: HostInputEvent) {
        if let Some(handle) = self.0.hub.lock().as_ref() {
            handle.hub().publish_host(event);
        }
    }

    pub fn send_control(&self, message: snowglass_ipc::Message) -> bool {
        self.0.control.send(message)
    }

    pub fn open_raw_hid(
        &self,
        index: snowglass_input::PlayerIndex,
        spec: RawHidSpec,
    ) -> io::Result<RawHidChannel> {
        let channel = RawHidChannel::open(index, spec, self.0.hub.clone())?;
        self.0.raw_hid.lock().insert(index, channel.clone());
        Ok(channel)
    }

    pub fn shutdown(&self) {
        if self.0.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.0.listener.close();
        self.0.control.shutdown();
        if let Some(worker) = self.0.worker.lock().take() {
            let _ = worker.join();
        }
        self.0.raw_hid.lock().clear();
        self.0.hub.lock().take();
    }

    fn accept(&self, socket: &mut snowglass_ipc::LocalSocket) -> io::Result<()> {
        self.0.publication.lock().accept(socket)?;
        self.0.control.add_client(socket, self.0.hub.clone())
    }
}

impl InputListener for InputIpcListener {
    fn on_input(&self, event: InputEvent<'_>) {
        if let InputEvent::RawHid { index, report, .. } = event {
            if let Some(channel) = self.0.raw_hid.lock().get(&index) {
                channel.publish(report);
            }
        }
        self.0.publication.lock().on_input(event);
    }

    fn wants_raw_hid(&self, device: &snowglass_input::DeviceDescriptor) -> bool {
        device
            .capabilities
            .contains(snowglass_input::Capabilities::RAW_HID)
    }
}

impl HostInputListener for InputIpcListener {
    fn on_host_input(&self, event: &HostInputEvent) {
        self.0.publication.lock().on_host_input(event);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.listener.close();
        self.control.shutdown();
        if let Some(worker) = self.worker.get_mut().take() {
            let _ = worker.join();
        }
    }
}
