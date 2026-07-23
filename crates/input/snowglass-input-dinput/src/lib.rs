//! DirectInput message production and target-side forwarded input state.

#![allow(unsafe_code)]

use snowglass_input::{DirectInputData, DirectInputDevice};
use snowglass_ipc::{Message, input::InputSubscriber};
use snowglass_platform_windows::{NativeWindow, forward_raw_key};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
        mpsc,
    },
    time::Instant,
};

const BUFFER_CAPACITY: usize = 1024;

/// Snapshot returned for `DIMOUSESTATE` and `DIMOUSESTATE2` polling.
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
}

impl InputQueues {
    fn new() -> Self {
        Self {
            keyboard: VecDeque::new(),
            mouse: VecDeque::new(),
            keyboard_state: [0; 256],
            mouse_state: MouseState::default(),
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
            DirectInputDevice::Mouse => match event.offset {
                0 | 4 | 8 => {
                    let axis = (event.offset / 4) as usize;
                    self.mouse_state.axes[axis] =
                        self.mouse_state.axes[axis].wrapping_add(event.data as i32);
                }
                12..=19 => {
                    self.mouse_state.buttons[(event.offset - 12) as usize] = event.data as u8;
                }
                _ => {}
            },
            _ => {}
        }
        let queue = self.get_mut(event.device);
        if queue.len() == BUFFER_CAPACITY {
            queue.pop_front();
        }
        queue.push_back(event);
    }
}

impl Default for InputQueues {
    fn default() -> Self {
        Self::new()
    }
}

type Logger = fn(&str);

fn discard_log(_: &str) {}

/// Receives the shared input stream, maintains DirectInput state, and routes
/// normalized pointer messages through the Win32 window-message backend.
pub struct Runtime {
    receiver_started: AtomicBool,
    connected: AtomicBool,
    logged_keyboard_receive: AtomicBool,
    logged_mouse_receive: AtomicBool,
    queues: Mutex<InputQueues>,
    notifications: Mutex<HashMap<usize, (DirectInputDevice, usize)>>,
    log: Logger,
}

impl Runtime {
    pub fn new(log: Logger) -> Self {
        Self {
            receiver_started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            logged_keyboard_receive: AtomicBool::new(false),
            logged_mouse_receive: AtomicBool::new(false),
            queues: Mutex::new(InputQueues::new()),
            notifications: Mutex::new(HashMap::new()),
            log,
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

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub fn keyboard_state(&self) -> [u8; 256] {
        self.queues
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keyboard_state
    }

    /// Returns the accumulated relative mouse state and consumes its axes.
    pub fn take_mouse_state(&self) -> MouseState {
        let mut queues = self
            .queues
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let state = queues.mouse_state;
        queues.mouse_state.axes = [0; 3];
        state
    }

    pub fn buffered_len(&self, device: DirectInputDevice) -> usize {
        self.queues
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(device)
            .len()
    }

    pub fn buffered_data(
        &self,
        device: DirectInputDevice,
        requested: usize,
        peek: bool,
    ) -> Vec<DirectInputData> {
        let mut queues = self
            .queues
            .lock()
            .unwrap_or_else(|error| error.into_inner());
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
        let mut notifications = self
            .notifications
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if event_handle == 0 {
            notifications.remove(&device_key);
        } else {
            notifications.insert(device_key, (device, event_handle));
        }
    }

    pub fn remove_device(&self, device_key: usize) {
        self.notifications
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&device_key);
    }

    fn receive(self: Arc<Self>, socket_name: String) {
        let Ok(mut input) = InputSubscriber::connect(&socket_name) else {
            (self.log)("snowglass: DirectInput input socket connection failed");
            return;
        };
        self.connected.store(true, Ordering::Release);
        (self.log)("snowglass: DirectInput input socket connected");
        while let Ok(message) = input.control.receive() {
            match message {
                Message::DirectInputData(event) => self.apply(event),
                Message::Shutdown => break,
                _ => {}
            }
        }
        self.connected.store(false, Ordering::Release);
    }

    fn apply(&self, event: DirectInputData) {
        if event.device == DirectInputDevice::Keyboard
            && !self.logged_keyboard_receive.swap(true, Ordering::AcqRel)
        {
            (self.log)(&format!(
                "snowglass: received first keyboard transition offset={:#x} data={:#x}",
                event.offset, event.data
            ));
        }
        if event.device == DirectInputDevice::Mouse
            && !self.logged_mouse_receive.swap(true, Ordering::AcqRel)
        {
            (self.log)(&format!(
                "snowglass: received first mouse transition offset={} data={}",
                event.offset, event.data as i32
            ));
        }
        self.queues
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .apply(event);
        let handles = self
            .notifications
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .filter(|(device, _)| *device == event.device)
            .map(|(_, handle)| *handle)
            .collect::<Vec<_>>();
        for handle in handles {
            unsafe { windows_sys::Win32::System::Threading::SetEvent(handle as _) };
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(discard_log)
    }
}

/// Builds and sends buffered DirectInput mouse records.
pub struct DirectInputForwarder {
    sender: mpsc::Sender<Message>,
    sequence: u32,
    started: Instant,
    mouse_position: Option<(u64, i32, i32, u32, u32)>,
}

impl DirectInputForwarder {
    pub fn new(sender: mpsc::Sender<Message>) -> Self {
        Self {
            sender,
            sequence: 0,
            started: Instant::now(),
            mouse_position: None,
        }
    }

    pub fn mouse_button(&mut self, button: u16, pressed: bool) {
        self.send(
            DirectInputDevice::Mouse,
            12 + u32::from(button.saturating_sub(1)),
            if pressed { 0x80 } else { 0 },
        );
    }

    pub fn mouse_wheel(&mut self, vertical: i32) {
        if vertical != 0 {
            self.send(DirectInputDevice::Mouse, 8, vertical as u32);
        }
    }

    /// Produces relative DirectInput axes from an absolute position already
    /// mapped into the target's presentation coordinates.
    pub fn mouse_position(&mut self, surface_id: u64, x: i32, y: i32, width: u32, height: u32) {
        let previous = self
            .mouse_position
            .replace((surface_id, x, y, width, height));
        let Some((previous_surface, previous_x, previous_y, previous_width, previous_height)) =
            previous
        else {
            return;
        };
        if previous_surface != surface_id || previous_width != width || previous_height != height {
            return;
        }
        for (offset, value) in [
            (0, x.saturating_sub(previous_x)),
            (4, y.saturating_sub(previous_y)),
        ] {
            if value != 0 {
                self.send(DirectInputDevice::Mouse, offset, value as u32);
            }
        }
    }

    pub fn reset_mouse_position(&mut self) {
        self.mouse_position = None;
    }

    pub fn release_mouse_buttons(&mut self, buttons: u32) {
        for index in 0..32 {
            if buttons & (1 << index) != 0 {
                self.send(DirectInputDevice::Mouse, 12 + index, 0);
            }
        }
    }

    fn send(&mut self, device: DirectInputDevice, offset: u32, data: u32) {
        self.sequence = self.sequence.wrapping_add(1);
        let _ = self.sender.send(Message::DirectInputData(DirectInputData {
            device,
            offset,
            data,
            timestamp: self.started.elapsed().as_millis() as u32,
            sequence: self.sequence,
        }));
    }
}

struct RawKeyboardContext {
    sender: mpsc::Sender<Message>,
    backing_window: Arc<AtomicIsize>,
    sequence: AtomicU32,
    started: Instant,
    pressed: [bool; 256],
}

/// Parses `WM_INPUT` keyboard messages and sends DirectInput records.
pub struct RawKeyboardForwarder {
    window: isize,
    context: *mut RawKeyboardContext,
}

impl RawKeyboardForwarder {
    pub fn install(
        window: NativeWindow,
        sender: mpsc::Sender<Message>,
        backing_window: Arc<AtomicIsize>,
    ) -> std::io::Result<Self> {
        use windows_sys::Win32::UI::{
            Input::{RAWINPUTDEVICE, RIDEV_INPUTSINK, RegisterRawInputDevices},
            Shell::SetWindowSubclass,
        };
        let context = Box::into_raw(Box::new(RawKeyboardContext {
            sender,
            backing_window,
            sequence: AtomicU32::new(0),
            started: Instant::now(),
            pressed: [false; 256],
        }));
        let device = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 6,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window.0 as _,
        };
        if unsafe {
            RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        } == 0
            || unsafe {
                SetWindowSubclass(
                    window.0 as _,
                    Some(raw_keyboard_subclass),
                    context as usize,
                    context as usize,
                )
            } == 0
        {
            unsafe { drop(Box::from_raw(context)) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            window: window.0,
            context,
        })
    }
}

impl Drop for RawKeyboardForwarder {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;
        unsafe {
            RemoveWindowSubclass(
                self.window as _,
                Some(raw_keyboard_subclass),
                self.context as usize,
            );
            drop(Box::from_raw(self.context));
        }
    }
}

unsafe extern "system" fn raw_keyboard_subclass(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    context: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::{
        Input::{GetRawInputData, RAWINPUT, RID_INPUT, RIM_TYPEKEYBOARD},
        Shell::DefSubclassProc,
        WindowsAndMessaging::{RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1, WM_INPUT},
    };
    if message == WM_INPUT {
        let mut input = RAWINPUT::default();
        let mut size = std::mem::size_of::<RAWINPUT>() as u32;
        if unsafe {
            GetRawInputData(
                lparam as _,
                RID_INPUT,
                (&mut input as *mut RAWINPUT).cast(),
                &mut size,
                std::mem::size_of_val(&input.header) as u32,
            )
        } != u32::MAX
            && input.header.dwType == RIM_TYPEKEYBOARD
        {
            let keyboard = unsafe { input.data.keyboard };
            if keyboard.MakeCode != 0 {
                let extended = keyboard.Flags & (RI_KEY_E0 | RI_KEY_E1) as u16 != 0;
                let offset = u32::from(keyboard.MakeCode) | u32::from(extended) * 0x80;
                let context = unsafe { &mut *(context as *mut RawKeyboardContext) };
                if let Some(previous) = context.pressed.get_mut(offset as usize) {
                    let pressed = keyboard.Flags & RI_KEY_BREAK as u16 == 0;
                    let repeat = pressed && *previous;
                    *previous = pressed;
                    let sequence = context.sequence.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = context
                        .sender
                        .send(Message::DirectInputData(DirectInputData {
                            device: DirectInputDevice::Keyboard,
                            offset,
                            data: if pressed { 0x80 } else { 0 },
                            timestamp: context.started.elapsed().as_millis() as u32,
                            sequence,
                        }));
                    let backing = context.backing_window.load(Ordering::Acquire);
                    if backing != 0 {
                        forward_raw_key(
                            NativeWindow(backing),
                            u32::from(keyboard.VKey),
                            keyboard.MakeCode,
                            extended,
                            pressed,
                            repeat,
                        );
                    }
                }
            }
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(device: DirectInputDevice, offset: u32, data: u32) -> DirectInputData {
        DirectInputData {
            device,
            offset,
            data,
            timestamp: 12,
            sequence: 34,
        }
    }

    #[test]
    fn runtime_instances_do_not_share_input() {
        let first = Runtime::default();
        let second = Runtime::default();
        first.apply(event(DirectInputDevice::Keyboard, 0x1e, 0x80));
        assert_eq!(first.keyboard_state()[0x1e], 0x80);
        assert_eq!(second.keyboard_state()[0x1e], 0);
    }

    #[test]
    fn mouse_axes_are_accumulated_then_consumed() {
        let runtime = Runtime::default();
        runtime.apply(event(DirectInputDevice::Mouse, 0, 12));
        runtime.apply(event(DirectInputDevice::Mouse, 0, (-2i32) as u32));
        assert_eq!(runtime.take_mouse_state().axes[0], 10);
        assert_eq!(runtime.take_mouse_state().axes[0], 0);
    }

    #[test]
    fn buffered_reads_support_peeking() {
        let runtime = Runtime::default();
        let first = event(DirectInputDevice::Keyboard, 1, 0x80);
        let second = event(DirectInputDevice::Keyboard, 2, 0x80);
        runtime.apply(first);
        runtime.apply(second);
        assert_eq!(
            runtime.buffered_data(DirectInputDevice::Keyboard, 1, true),
            vec![first]
        );
        assert_eq!(
            runtime.buffered_data(DirectInputDevice::Keyboard, 2, false),
            vec![first, second]
        );
        assert_eq!(runtime.buffered_len(DirectInputDevice::Keyboard), 0);
    }

    #[test]
    fn mapped_mouse_positions_become_relative_axes() {
        let (sender, receiver) = mpsc::channel();
        let mut forwarder = DirectInputForwarder::new(sender);
        forwarder.mouse_position(7, 100, 200, 640, 480);
        assert!(receiver.try_recv().is_err());

        forwarder.mouse_position(7, 112, 195, 640, 480);
        let Message::DirectInputData(horizontal) = receiver.recv().unwrap() else {
            panic!("expected horizontal DirectInput data");
        };
        let Message::DirectInputData(vertical) = receiver.recv().unwrap() else {
            panic!("expected vertical DirectInput data");
        };
        assert_eq!((horizontal.offset, horizontal.data), (0, 12));
        assert_eq!((vertical.offset, vertical.data), (4, (-5i32) as u32));
    }

    #[test]
    fn mapped_mouse_rebases_when_the_surface_changes() {
        let (sender, receiver) = mpsc::channel();
        let mut forwarder = DirectInputForwarder::new(sender);
        forwarder.mouse_position(7, 100, 200, 640, 480);
        forwarder.mouse_position(8, 500, 400, 1280, 720);
        assert!(receiver.try_recv().is_err());
    }
}
