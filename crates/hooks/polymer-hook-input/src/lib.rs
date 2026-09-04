mod host;
mod state;
mod subscriber;
mod topology;

pub use host::HostState;
pub use topology::TopologyGuard;

use parking_lot::Mutex;
use polymer_input::{DeviceState, Feedback, GamepadReport};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct HookInput {
    state: state::InputState,
    host: HostState,
    topology: topology::TopologyState,
    control: subscriber::ControlSender,
    connected: AtomicBool,
    controls: Mutex<VecDeque<polymer_ipc::Message>>,
}

impl HookInput {
    pub fn start(session: &str) -> Arc<Self> {
        let (control, receiver) = subscriber::ControlSender::new();
        let value = Arc::new(Self {
            state: state::InputState::default(),
            host: HostState::default(),
            topology: topology::TopologyState::default(),
            control,
            connected: AtomicBool::new(false),
            controls: Mutex::new(VecDeque::new()),
        });
        subscriber::Subscriber::spawn(format!("{session}-input"), value.clone(), receiver);
        value
    }

    pub fn topology(&self) -> TopologyGuard<'_> {
        self.topology.read()
    }

    pub fn mapped(&self, player: u8) -> Option<GamepadReport> {
        let state = self.raw(player)?;
        self.topology
            .map(player, &state.gamepad, &self.host.sample())
    }

    pub fn raw(&self, player: u8) -> Option<DeviceState> {
        self.state.get(player)
    }

    pub fn resolved(&self, player: u8) -> Option<DeviceState> {
        let mut state = self.raw(player)?;
        let host = self.host.sample();
        state.pointer = self.topology.pointer(player, state.pointer, &host);
        Some(state)
    }

    pub fn host(&self) -> &HostState {
        &self.host
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub fn drain_control(&self) -> Vec<polymer_ipc::Message> {
        self.controls.lock().drain(..).collect()
    }

    pub fn send_feedback(&self, player: u8, feedback: Feedback) -> bool {
        self.control.feedback(player, feedback)
    }

    pub(crate) fn receive(&self, event: polymer_ipc::input::InputMessage) {
        self.state.receive(event);
        self.host.receive(event);
    }

    pub(crate) fn set_topology(&self, value: polymer_input_layout::topology::InputTopology) {
        self.topology.set(value);
    }

    pub(crate) fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Release);
    }

    pub(crate) fn receive_control(&self, message: polymer_ipc::Message) {
        let mut controls = self.controls.lock();
        if controls.len() == 64 {
            controls.pop_front();
        }
        controls.push_back(message);
    }
}
