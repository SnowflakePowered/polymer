//! SDL-version-neutral virtual gamepad state and forwarding runtime.

use seqlock::SeqLock;
use snowglass_input::gamepad::{GamepadState, GamepadUpdate};
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
    received: u32,
    sequence: u32,
    connected: u32,
    buttons: u32,
    left_stick: [i16; 2],
    right_stick: [i16; 2],
    left_trigger: u16,
    right_trigger: u16,
    pointer: [i16; 2],
    accelerometer: [f32; 3],
    gyroscope: [f32; 3],
}

impl From<GamepadUpdate> for SlotState {
    fn from(update: GamepadUpdate) -> Self {
        Self {
            received: 1,
            sequence: update.sequence,
            connected: u32::from(update.connected),
            buttons: update.state.buttons,
            left_stick: update.state.left_stick,
            right_stick: update.state.right_stick,
            left_trigger: update.state.left_trigger,
            right_trigger: update.state.right_trigger,
            pointer: update.state.pointer,
            accelerometer: update.state.accelerometer,
            gyroscope: update.state.gyroscope,
        }
    }
}

impl SlotState {
    fn update(self, index: usize) -> GamepadUpdate {
        GamepadUpdate {
            slot: snowglass_input::gamepad::GamepadSlot::new(index as u8)
                .expect("slot index is representable on the wire"),
            sequence: self.sequence,
            connected: self.connected != 0,
            state: GamepadState {
                buttons: self.buttons,
                left_stick: self.left_stick,
                right_stick: self.right_stick,
                left_trigger: self.left_trigger,
                right_trigger: self.right_trigger,
                pointer: self.pointer,
                accelerometer: self.accelerometer,
                gyroscope: self.gyroscope,
            },
        }
    }
}

pub struct Runtime {
    receiver_started: AtomicBool,
    pad_count: usize,
    slots: Vec<SeqLock<SlotState>>,
    published_sequences: Mutex<Vec<Option<u32>>>,
}

impl Runtime {
    pub fn new(pad_count: usize) -> Self {
        let slots = (0..pad_count.clamp(1, u8::MAX as usize + 1))
            .map(|_| SeqLock::new(SlotState::default()))
            .collect();
        Self {
            receiver_started: AtomicBool::new(false),
            pad_count: pad_count.clamp(1, u8::MAX as usize + 1),
            slots,
            published_sequences: Mutex::new(vec![None; pad_count.clamp(1, u8::MAX as usize + 1)]),
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
        let mut published = self
            .published_sequences
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (index, slot) in self.slots.iter().enumerate() {
            let slot = slot.read();
            if slot.received == 0 || published[index] == Some(slot.sequence) {
                continue;
            }
            let update = slot.update(index);
            if backend.publish(update).is_ok() {
                published[index] = Some(slot.sequence);
            }
        }
    }

    fn apply(&self, update: GamepadUpdate) {
        if update.slot.index() >= self.pad_count {
            return;
        }
        let current = self.slots[update.slot.index()].read();
        if current.received != 0 && !sequence_is_newer(update.sequence, current.sequence) {
            return;
        }
        *self.slots[update.slot.index()].lock_write() = SlotState::from(update);
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
        assert_eq!(Runtime::new(256).pad_count, 256);
        assert_eq!(Runtime::new(257).pad_count, 256);
    }

    #[test]
    fn sequence_comparison_wraps() {
        assert!(sequence_is_newer(0, u32::MAX));
        assert!(!sequence_is_newer(u32::MAX, 0));
    }
}
