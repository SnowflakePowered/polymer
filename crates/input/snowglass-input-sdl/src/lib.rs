//! SDL-version-neutral virtual gamepad state and forwarding runtime.

use snowglass_input::gamepad::{GamepadState, GamepadUpdate, MAX_GAMEPADS};
use snowglass_ipc::{LocalSocket, Message};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendError(pub &'static str);

/// The common surface implemented by each SDL major-version hook.
pub trait SdlBackend {
    fn ensure_attached(&mut self, count: usize) -> Result<(), BackendError>;
    fn publish(&mut self, update: GamepadUpdate) -> Result<(), BackendError>;
}

/// Converts a disconnected update to a neutral state while retaining the
/// process-lifetime virtual device topology.
pub fn effective_state(update: GamepadUpdate) -> GamepadState {
    if update.connected {
        update.state
    } else {
        GamepadState::default()
    }
}

#[derive(Clone, Copy, Default)]
struct SlotState {
    received: bool,
    dirty: bool,
    sequence: u32,
    connected: bool,
    state: GamepadState,
}

pub struct Runtime {
    receiver_started: AtomicBool,
    pad_count: usize,
    slots: Mutex<[SlotState; MAX_GAMEPADS]>,
}

impl Runtime {
    pub fn new(pad_count: usize) -> Self {
        Self {
            receiver_started: AtomicBool::new(false),
            pad_count: pad_count.clamp(1, MAX_GAMEPADS),
            slots: Mutex::new([SlotState::default(); MAX_GAMEPADS]),
        }
    }

    pub fn start_receiver(self: &Arc<Self>, socket_name: String) {
        if self
            .receiver_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let runtime = self.clone();
        std::thread::spawn(move || runtime.receive(socket_name));
    }

    pub fn prepare_enumeration(&self, backend: &mut impl SdlBackend) -> Result<(), BackendError> {
        backend.ensure_attached(self.pad_count)
    }

    pub fn flush(&self, backend: &mut impl SdlBackend) {
        if backend.ensure_attached(self.pad_count).is_err() {
            return;
        }
        let pending = {
            let slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
            slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.dirty)
                .filter_map(|(index, slot)| {
                    Some(GamepadUpdate {
                        slot: snowglass_input::gamepad::GamepadSlot::new(index as u8)?,
                        sequence: slot.sequence,
                        connected: slot.connected,
                        state: slot.state,
                    })
                })
                .collect::<Vec<_>>()
        };
        for update in pending {
            if backend.publish(update).is_ok() {
                let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
                let slot = &mut slots[update.slot.index()];
                if slot.sequence == update.sequence {
                    slot.dirty = false;
                }
            }
        }
    }

    fn apply(&self, update: GamepadUpdate) {
        if update.slot.index() >= self.pad_count {
            return;
        }
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let slot = &mut slots[update.slot.index()];
        if slot.received && !sequence_is_newer(update.sequence, slot.sequence) {
            return;
        }
        *slot = SlotState {
            received: true,
            dirty: true,
            sequence: update.sequence,
            connected: update.connected,
            state: update.state,
        };
    }

    fn receive(self: Arc<Self>, socket_name: String) {
        let Ok(mut socket) = LocalSocket::connect(&socket_name) else {
            return;
        };
        while let Ok(message) = socket.receive() {
            match message {
                Message::GamepadUpdate(update) => self.apply(update),
                Message::Shutdown => break,
                _ => {}
            }
        }
    }
}

fn sequence_is_newer(candidate: u32, current: u32) -> bool {
    let distance = candidate.wrapping_sub(current);
    distance != 0 && distance < (1 << 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_input::gamepad::GamepadSlot;

    #[derive(Default)]
    struct FakeBackend {
        attached: usize,
        published: Vec<GamepadUpdate>,
    }

    impl SdlBackend for FakeBackend {
        fn ensure_attached(&mut self, count: usize) -> Result<(), BackendError> {
            self.attached = count;
            Ok(())
        }

        fn publish(&mut self, update: GamepadUpdate) -> Result<(), BackendError> {
            self.published.push(update);
            Ok(())
        }
    }

    fn update(sequence: u32) -> GamepadUpdate {
        GamepadUpdate {
            slot: GamepadSlot::new(0).unwrap(),
            sequence,
            connected: true,
            state: GamepadState {
                buttons: sequence,
                ..GamepadState::default()
            },
        }
    }

    #[test]
    fn flushes_only_the_latest_snapshot() {
        let runtime = Runtime::new(1);
        runtime.apply(update(10));
        runtime.apply(update(9));
        runtime.apply(update(11));
        let mut backend = FakeBackend::default();
        runtime.flush(&mut backend);
        runtime.flush(&mut backend);
        assert_eq!(backend.attached, 1);
        assert_eq!(backend.published, vec![update(11)]);
    }

    #[test]
    fn enumeration_attaches_virtual_gamepads_without_waiting_for_input() {
        let runtime = Runtime::new(3);
        let mut backend = FakeBackend::default();
        runtime.prepare_enumeration(&mut backend).unwrap();
        assert_eq!(backend.attached, 3);
        assert!(backend.published.is_empty());
    }

    #[test]
    fn clamps_topology_to_supported_slots() {
        assert_eq!(Runtime::new(0).pad_count, 1);
        assert_eq!(Runtime::new(MAX_GAMEPADS + 1).pad_count, MAX_GAMEPADS);
    }

    #[test]
    fn sequence_comparison_wraps() {
        assert!(sequence_is_newer(0, u32::MAX));
        assert!(!sequence_is_newer(u32::MAX, 0));
    }
}
