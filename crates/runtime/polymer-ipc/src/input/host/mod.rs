//! Host input publication, topology updates, and raw HID forwarding.

mod control;
mod hid;
mod publisher;
mod topology;

pub use hid::{RawHidChannel, RawHidSpec};

use crate::LocalSocketListener;
use control::Control;
use parking_lot::Mutex;
use polymer_controllers::topology::InputTopology;
use polymer_input::{
    DeviceState, HostInputEvent, HostInputListener, HostListenerHandle, InputEvent, InputHub,
    InputListener, ListenerHandle, PlayerIndex,
};
use publisher::InputPublication;
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
    raw_hid: Mutex<BTreeMap<polymer_input::PlayerIndex, RawHidChannel>>,
    forwarding: Mutex<ForwardingState>,
}

#[derive(Default)]
struct ForwardingState {
    enabled: bool,
    connected: BTreeSet<PlayerIndex>,
    blocked_until_neutral: BTreeSet<PlayerIndex>,
    holding: BTreeSet<PlayerIndex>,
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

        // Raw streams remain captured until the corresponding normalized state
        // is neutral. The Bluetooth decoder is the sole button-state authority.
        for (&index, channel) in raw_hid.iter() {
            channel.set_capture(true);
            forwarding.blocked_until_neutral.insert(index);
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
            } else {
                forwarding.blocked_until_neutral.remove(&index);
                if let Some(channel) = raw_hid.get(&index) {
                    channel.set_capture(false);
                }
            }
        }
    }

    pub fn publish_host(&self, event: HostInputEvent) {
        if let Some(handle) = self.0.hub.lock().as_ref() {
            handle.hub().publish_host(event);
        }
    }

    /// Publishes an action keystroke directly to the emulator hook even while
    /// ordinary host input is suspended for an overlay.
    pub fn publish_host_action(&self, event: HostInputEvent) {
        self.0.publication.lock().on_host_input(&event);
    }

    /// Publishes an action-only SDL chord without consulting guest forwarding.
    pub fn activate_buttons(&self, player: PlayerIndex, buttons: u32) -> Result<(), String> {
        const HOLD_TIME: std::time::Duration = std::time::Duration::from_millis(50);
        let hub = self
            .0
            .hub
            .lock()
            .as_ref()
            .map(|handle| handle.hub().clone())
            .ok_or_else(|| "input action endpoint is not attached".to_owned())?;
        let device = hub
            .devices()
            .into_iter()
            .find_map(|(index, device)| (index == Some(player)).then_some(device))
            .ok_or_else(|| format!("input action player {} is not connected", player.get()))?;
        let enabled = {
            let mut forwarding = self.0.forwarding.lock();
            forwarding.holding.insert(player);
            forwarding.enabled
        };
        let mut publication = self.0.publication.lock();
        if !enabled {
            publication.connect(player, &device);
        }
        publication.publish_action(player, buttons);
        drop(publication);
        std::thread::sleep(HOLD_TIME);
        let mut forwarding = self.0.forwarding.lock();
        forwarding.holding.remove(&player);
        let mut publication = self.0.publication.lock();
        let live = forwarding.enabled && !forwarding.blocked_until_neutral.contains(&player);
        match hub.state(player).filter(|_| live) {
            Some(state) => publication.publish_state(player, &state),
            None => publication.publish_action(player, 0),
        }
        if !forwarding.enabled {
            publication.disconnect(player);
        }
        Ok(())
    }

    pub fn send_control(&self, message: crate::Message) -> bool {
        self.0.control.send(message)
    }

    pub fn open_raw_hid(
        &self,
        index: polymer_input::PlayerIndex,
        spec: RawHidSpec,
    ) -> io::Result<RawHidChannel> {
        let channel = RawHidChannel::open(index, spec, self.0.hub.clone())?;
        let hub = self
            .0
            .hub
            .lock()
            .as_ref()
            .map(|handle| handle.hub().clone());
        let mut forwarding = self.0.forwarding.lock();
        channel.set_capture(true);
        if forwarding.enabled {
            let neutral = hub
                .and_then(|hub| hub.state(index))
                .is_some_and(|state| state.gamepad.buttons == 0);
            if neutral {
                channel.set_capture(false);
            } else {
                forwarding.blocked_until_neutral.insert(index);
            }
        }
        self.0.raw_hid.lock().insert(index, channel.clone());
        Ok(channel)
    }

    pub fn clear_raw_hid(&self) {
        self.0.raw_hid.lock().clear();
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

    fn serve(self, mut socket: crate::LocalSocket) {
        let accepted = socket
            .try_clone()
            .and_then(|shutdown| {
                self.0
                    .control
                    .handshake(shutdown, || self.accept(&mut socket))
            })
            .and_then(|()| self.0.control.add_client(&socket));
        if let Err(error) = accepted {
            tracing::warn!("input client rejected: {error}");
            return;
        }
        let feedback = self.0.hub.clone();
        drop(self);
        Control::forward_feedback(socket, &feedback);
    }

    fn accept(&self, socket: &mut crate::LocalSocket) -> io::Result<()> {
        let crate::Message::Hello(hello) = socket.receive()? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected input hello",
            ));
        };
        let offer = self.0.publication.lock().map_offer(&hello)?;
        socket.send(crate::Message::MapOffer(offer))?;
        if socket.receive()? != crate::Message::MapAccepted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "input map was not accepted",
            ));
        }
        self.0.publication.lock().replay()
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
                if forwarding.holding.contains(index) || !forwarding.accepts(*index, state) {
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

    fn wants_raw_hid(&self, device: &polymer_input::DeviceDescriptor) -> bool {
        device
            .capabilities
            .contains(polymer_input::Capabilities::RAW_HID)
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
