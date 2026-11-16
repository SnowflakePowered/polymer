#![allow(unsafe_code)]

use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use polymer_hook_input::HookInput;
use polymer_ipc::input::{InputMessage, WireHostKey, WireHostPointer};
use polymer_protocol::input::{DirectInputData, DirectInputDevice};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

const BUFFER_CAPACITY: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MouseState {
    pub axes: [i32; 3],
    pub buttons: [u8; 8],
}

struct InputQueues {
    keyboard: VecDeque<DirectInputData>,
    mouse: VecDeque<DirectInputData>,
    keyboard_state: [u8; 256],
    mouse_state: MouseState,
    pointer_position: Option<[f32; 2]>,
}

impl InputQueues {
    fn new() -> Self {
        Self {
            keyboard: VecDeque::new(),
            mouse: VecDeque::new(),
            keyboard_state: [0; 256],
            mouse_state: MouseState::default(),
            pointer_position: None,
        }
    }

    fn get(&self, device: DirectInputDevice) -> &VecDeque<DirectInputData> {
        match device {
            DirectInputDevice::Keyboard => &self.keyboard,
            DirectInputDevice::Mouse => &self.mouse,
        }
    }

    fn get_mut(&mut self, device: DirectInputDevice) -> &mut VecDeque<DirectInputData> {
        match device {
            DirectInputDevice::Keyboard => &mut self.keyboard,
            DirectInputDevice::Mouse => &mut self.mouse,
        }
    }

    fn apply(&mut self, event: DirectInputData) {
        match event.device {
            DirectInputDevice::Keyboard if (event.offset as usize) < self.keyboard_state.len() => {
                self.keyboard_state[event.offset as usize] = event.data as u8;
            }
            DirectInputDevice::Mouse => self.apply_mouse(event),
            _ => {}
        }
        let queue = self.get_mut(event.device);
        if queue.len() == BUFFER_CAPACITY {
            queue.pop_front();
        }
        queue.push_back(event);
    }

    fn apply_mouse(&mut self, event: DirectInputData) {
        match event.offset {
            0 | 4 | 8 => {
                let axis = (event.offset / 4) as usize;
                self.mouse_state.axes[axis] =
                    self.mouse_state.axes[axis].wrapping_add(event.data as i32);
            }
            12..=19 => self.mouse_state.buttons[(event.offset - 12) as usize] = event.data as u8,
            _ => {}
        }
    }
}

impl Default for InputQueues {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Runtime {
    receiver_started: AtomicBool,
    input: RwLock<Option<Arc<HookInput>>>,
    queues: Mutex<InputQueues>,
    notifications: DashMap<usize, (DirectInputDevice, usize)>,
    mouse_sequence: AtomicU32,
}

impl Runtime {
    pub fn new() -> Self {
        Self {
            receiver_started: AtomicBool::new(false),
            input: RwLock::new(None),
            queues: Mutex::new(InputQueues::new()),
            notifications: DashMap::new(),
            mouse_sequence: AtomicU32::new(0),
        }
    }

    pub fn start_receiver(self: &Arc<Self>, socket_name: String) {
        if self.receiver_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let session = socket_name.strip_suffix("-input").unwrap_or(&socket_name);
        *self.input.write() = Some(HookInput::start(session));
        let runtime = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("polymer-dinput-state".to_owned())
            .spawn(move || {
                while let Some(runtime) = runtime.upgrade() {
                    runtime.synchronize();
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
    }

    pub fn connected(&self) -> bool {
        self.input
            .read()
            .as_ref()
            .is_some_and(|input| input.connected())
    }

    pub fn keyboard_state(&self) -> [u8; 256] {
        self.synchronize();
        self.queues.lock().keyboard_state
    }

    pub fn take_mouse_state(&self) -> MouseState {
        self.synchronize();
        let mut queues = self.queues.lock();
        let state = queues.mouse_state;
        queues.mouse_state.axes = [0; 3];
        state
    }

    pub fn buffered_len(&self, device: DirectInputDevice) -> usize {
        self.synchronize();
        self.queues.lock().get(device).len()
    }

    pub fn buffered_data(
        &self,
        device: DirectInputDevice,
        requested: usize,
        peek: bool,
    ) -> Vec<DirectInputData> {
        self.synchronize();
        let mut queues = self.queues.lock();
        let queue = queues.get_mut(device);
        let available = queue.len().min(requested);
        if peek {
            queue.iter().take(available).copied().collect()
        } else {
            queue.drain(..available).collect()
        }
    }

    pub fn set_event_notification(
        &self,
        device_key: usize,
        device: DirectInputDevice,
        event_handle: usize,
    ) {
        if event_handle == 0 {
            self.notifications.remove(&device_key);
        } else {
            self.notifications
                .insert(device_key, (device, event_handle));
        }
    }

    pub fn remove_device(&self, device_key: usize) {
        self.notifications.remove(&device_key);
    }

    pub fn shutdown(&self) {
        self.input.write().take();
    }

    fn synchronize(&self) {
        let events = self
            .input
            .read()
            .as_ref()
            .map(|input| input.host().drain())
            .unwrap_or_default();
        for event in events {
            match event {
                InputMessage::HostKey(key) => self.apply_key(key),
                InputMessage::HostPointer(pointer) => self.apply_pointer(pointer),
                InputMessage::HostWheel(wheel) if wheel.vertical != 0 => {
                    self.apply(self.mouse_event(8, wheel.vertical as u32, 0));
                }
                _ => {}
            }
        }
    }

    fn apply_key(&self, key: WireHostKey) {
        let offset = u32::from(key.scan_code) | (u32::from(key.extended) * 0x80);
        self.apply(DirectInputData {
            device: DirectInputDevice::Keyboard,
            offset,
            data: if key.pressed { 0x80 } else { 0 },
            timestamp: key.timestamp_ms,
            sequence: key.sequence,
        });
    }

    fn apply_pointer(&self, pointer: WireHostPointer) {
        let (delta, changed_buttons) = {
            let mut queues = self.queues.lock();
            let previous = queues.pointer_position.replace(pointer.position);
            let delta = if pointer.delta != [0.0; 2] {
                pointer.delta
            } else {
                previous.map_or([0.0; 2], |previous| {
                    [
                        (pointer.position[0] - previous[0]) * 32767.0,
                        (pointer.position[1] - previous[1]) * 32767.0,
                    ]
                })
            };
            let current = queues
                .mouse_state
                .buttons
                .iter()
                .enumerate()
                .fold(0, |buttons, (index, pressed)| {
                    buttons | (u32::from(*pressed != 0) << index)
                });
            (delta, current ^ pointer.buttons)
        };
        for (offset, value) in [(0, delta[0]), (4, delta[1])] {
            let value = value.round() as i32;
            if value != 0 {
                self.apply(self.mouse_event(offset, value as u32, pointer.timestamp_ms));
            }
        }
        for button in 0..8 {
            if changed_buttons & (1 << button) != 0 {
                self.apply(self.mouse_event(
                    12 + button,
                    if pointer.buttons & (1 << button) != 0 {
                        0x80
                    } else {
                        0
                    },
                    pointer.timestamp_ms,
                ));
            }
        }
    }

    fn mouse_event(&self, offset: u32, data: u32, timestamp: u32) -> DirectInputData {
        DirectInputData {
            device: DirectInputDevice::Mouse,
            offset,
            data,
            timestamp,
            sequence: self.mouse_sequence.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn apply(&self, event: DirectInputData) {
        self.queues.lock().apply(event);
        let handles = self
            .notifications
            .iter()
            .filter(|notification| notification.value().0 == event.device)
            .map(|notification| notification.value().1)
            .collect::<Vec<_>>();
        for handle in handles {
            unsafe { windows_sys::Win32::System::Threading::SetEvent(handle as _) };
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown();
    }
}
