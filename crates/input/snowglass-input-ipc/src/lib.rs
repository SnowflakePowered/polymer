mod control;
mod hid;
mod publisher;
mod topology;

pub use hid::{RawHidChannel, RawHidSpec};

use control::Control;
use parking_lot::Mutex;
use publisher::InputPublication;
use snowglass_input::{
    DeviceState, HostInputEvent, HostInputListener, HostListenerHandle, InputEvent, InputHub,
    InputListener, ListenerHandle, PlayerIndex,
};
use snowglass_input_layout::topology::InputTopology;
use snowglass_ipc::LocalSocketListener;
use std::collections::{BTreeMap, BTreeSet};
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
    forwarding: Mutex<ForwardingState>,
}

#[derive(Default)]
struct ForwardingState {
    enabled: bool,
    connected: BTreeSet<PlayerIndex>,
    blocked_until_neutral: BTreeSet<PlayerIndex>,
}

impl ForwardingState {
    fn accepts(&mut self, index: PlayerIndex, state: &DeviceState) -> bool {
        self.connected.insert(index);
        if !self.blocked_until_neutral.contains(&index) {
            return true;
        }
        if state.gamepad.buttons != 0 {
            return false;
        }
        self.blocked_until_neutral.remove(&index);
        true
    }
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
            forwarding: Mutex::new(ForwardingState::default()),
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

    /// Enables or suspends all guest-facing input publication.
    ///
    /// Resuming while a controller button is held keeps that player blocked
    /// until the release report arrives. This prevents the button used to
    /// dismiss an overlay from leaking through to the guest.
    pub fn set_forwarding(&self, enabled: bool, hub: &InputHub) {
        let snapshots = enabled.then(|| hub.device_snapshots());
        let mut forwarding = self.0.forwarding.lock();
        if forwarding.enabled == enabled {
            return;
        }

        let mut publication = self.0.publication.lock();
        let raw_hid = self.0.raw_hid.lock();
        forwarding.enabled = enabled;
        forwarding.blocked_until_neutral.clear();
        if !enabled {
            for channel in raw_hid.values() {
                channel.set_capture(true);
            }
            for index in std::mem::take(&mut forwarding.connected) {
                publication.disconnect(index);
            }
            return;
        }

        for (index, device, state, connected) in snapshots.into_iter().flatten() {
            let Some(index) = index.filter(|_| connected) else {
                continue;
            };
            publication.connect(index, &device);
            forwarding.connected.insert(index);
            let blocked = state.gamepad.buttons != 0;
            if blocked {
                forwarding.blocked_until_neutral.insert(index);
            }
            if let Some(channel) = raw_hid.get(&index) {
                channel.set_capture(blocked);
            }
        }
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
        let forwarding = self.0.forwarding.lock();
        channel
            .set_capture(!forwarding.enabled || forwarding.blocked_until_neutral.contains(&index));
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
            return;
        }

        let mut forwarding = self.0.forwarding.lock();
        if !forwarding.enabled {
            return;
        }
        match &event {
            InputEvent::Connected {
                index: Some(index), ..
            } => {
                forwarding.connected.insert(*index);
            }
            InputEvent::Disconnected {
                index: Some(index), ..
            } => {
                forwarding.connected.remove(index);
                forwarding.blocked_until_neutral.remove(index);
            }
            InputEvent::Assigned {
                from: Some(index), ..
            } => {
                forwarding.connected.remove(index);
                forwarding.blocked_until_neutral.remove(index);
                self.0.publication.lock().disconnect(*index);
            }
            InputEvent::State { index, state, .. } => {
                if !forwarding.accepts(*index, state) {
                    return;
                }
                if let Some(channel) = self.0.raw_hid.lock().get(index) {
                    channel.set_capture(false);
                }
            }
            _ => {}
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
        if self.0.forwarding.lock().enabled {
            self.0.publication.lock().on_host_input(event);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_input::GamepadButton;

    #[test]
    fn resuming_swallows_the_held_overlay_button_through_release() {
        let player = PlayerIndex::new(0);
        let mut forwarding = ForwardingState {
            enabled: true,
            blocked_until_neutral: BTreeSet::from([player]),
            ..Default::default()
        };
        let mut state = DeviceState::default();
        state.gamepad.buttons = GamepadButton::A.mask();

        assert!(!forwarding.accepts(player, &state));
        assert!(forwarding.connected.contains(&player));

        state.gamepad.buttons = 0;
        assert!(forwarding.accepts(player, &state));

        state.gamepad.buttons = GamepadButton::A.mask();
        assert!(forwarding.accepts(player, &state));
    }
}
