//! Win32 mouse window-message forwarding.

#![allow(unsafe_code)]

use parking_lot::Mutex;
use snowglass_input::{
    RawKeyInput,
    mouse::{PointerInput, PointerPhase},
};
use snowglass_ipc::{Message, input::InputSubscriber};
use snowglass_platform_windows::{
    NativeWindow, ProcessId, client_size, forward_raw_key, is_process_window, resize_client,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows_sys::Win32::{
    Foundation::{POINT, WPARAM},
    Graphics::Gdi::ClientToScreen,
    System::{
        SystemServices::{MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MK_XBUTTON1, MK_XBUTTON2},
        Threading::GetCurrentProcessId,
    },
    UI::WindowsAndMessaging::{
        PostMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
        XBUTTON1, XBUTTON2,
    },
};

/// Converts normalized pointer input into mouse messages for an HWND owned by
/// the current process.
#[derive(Default)]
pub struct WindowMessageForwarder {
    pointer_buttons: Mutex<HashMap<(u64, u32), u32>>,
}

type Logger = fn(&str);

fn discard_log(_: &str) {}

/// Receives normalized pointer input, forwards Win32 window messages, and
/// maintains the cursor position exposed by the Win32 hook.
pub struct Runtime {
    receiver_started: AtomicBool,
    connected: AtomicBool,
    logged_pointer_receive: AtomicBool,
    logged_pointer_delivery: AtomicBool,
    logged_pointer_rejection: AtomicBool,
    logged_key_receive: AtomicBool,
    logged_key_rejection: AtomicBool,
    cursor_position: Mutex<Option<(i32, i32)>>,
    window_messages: WindowMessageForwarder,
    log: Logger,
}

impl Runtime {
    pub fn new(log: Logger) -> Self {
        Self {
            receiver_started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            logged_pointer_receive: AtomicBool::new(false),
            logged_pointer_delivery: AtomicBool::new(false),
            logged_pointer_rejection: AtomicBool::new(false),
            logged_key_receive: AtomicBool::new(false),
            logged_key_rejection: AtomicBool::new(false),
            cursor_position: Mutex::new(None),
            window_messages: WindowMessageForwarder::default(),
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

    pub fn cursor_position(&self) -> Option<(i32, i32)> {
        *self.cursor_position.lock()
    }

    fn receive(self: Arc<Self>, socket_name: String) {
        let Ok(mut input) = InputSubscriber::connect(&socket_name) else {
            (self.log)("snowglass: Win32 input socket connection failed");
            return;
        };
        self.connected.store(true, Ordering::Release);
        (self.log)("snowglass: Win32 input socket connected");
        while let Ok(message) = input.control.receive() {
            match message {
                Message::PointerInput(event) => self.apply(event),
                Message::RawKeyInput(event) => self.apply_raw_key(event),
                Message::ResizeWindow(event) => self.apply_resize(event),
                Message::Shutdown => break,
                _ => {}
            }
        }
        self.connected.store(false, Ordering::Release);
    }

    fn apply(&self, event: PointerInput) {
        if let Some(position) = pointer_screen_position(event) {
            *self.cursor_position.lock() = Some(position);
        }
        if !self.logged_pointer_receive.swap(true, Ordering::AcqRel) {
            (self.log)(&format!(
                "snowglass: received first pointer hwnd={:#x} surface={} phase={:?} position=({}, {}) extent={}x{}",
                event.target_window,
                event.surface_id,
                event.phase,
                event.x,
                event.y,
                event.coordinate_width,
                event.coordinate_height,
            ));
        }
        if self.window_messages.apply(event) {
            if !self.logged_pointer_delivery.swap(true, Ordering::AcqRel) {
                (self.log)("snowglass: posted first pointer message to target window");
            }
        } else if !self.logged_pointer_rejection.swap(true, Ordering::AcqRel) {
            (self.log)(&format!(
                "snowglass: rejected pointer message for hwnd={:#x}; window is missing, invalid, foreign, or has an empty client area",
                event.target_window,
            ));
        }
    }

    fn apply_raw_key(&self, event: RawKeyInput) {
        let window = NativeWindow(event.target_window as isize);
        if event.target_window == 0
            || !is_process_window(window, ProcessId(unsafe { GetCurrentProcessId() }))
        {
            if !self.logged_key_rejection.swap(true, Ordering::AcqRel) {
                (self.log)(&format!(
                    "snowglass: rejected raw key for hwnd={:#x}; window is missing, invalid, or foreign",
                    event.target_window
                ));
            }
            return;
        }
        forward_raw_key(
            window,
            event.virtual_key,
            event.scan_code,
            event.extended,
            event.pressed,
            event.repeat,
        );
        if !self.logged_key_receive.swap(true, Ordering::AcqRel) {
            (self.log)(&format!(
                "snowglass: posted first raw key hwnd={:#x} vk={:#x} scan={:#x} pressed={}",
                event.target_window, event.virtual_key, event.scan_code, event.pressed
            ));
        }
    }

    fn apply_resize(&self, event: snowglass_protocol::ResizeWindow) {
        let window = NativeWindow(event.target_window as isize);
        if event.target_window != 0
            && is_process_window(window, ProcessId(unsafe { GetCurrentProcessId() }))
        {
            if let Err(error) = resize_client(window, event.width, event.height) {
                (self.log)(&format!(
                    "snowglass: failed to resize hwnd={:#x} to {}x{}: {error}",
                    event.target_window, event.width, event.height
                ));
            }
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(discard_log)
    }
}

fn pointer_screen_position(event: PointerInput) -> Option<(i32, i32)> {
    let window = NativeWindow(event.target_window as isize);
    let client = client_size(window)?;
    let mut point = POINT {
        x: scale_pointer_coordinate(event.x, event.coordinate_width, client.0),
        y: scale_pointer_coordinate(event.y, event.coordinate_height, client.1),
    };
    (unsafe { ClientToScreen(window.0 as _, &mut point) } != 0).then_some((point.x, point.y))
}

impl WindowMessageForwarder {
    /// Posts the messages represented by `event`, returning whether its target
    /// was a valid, non-empty window owned by this process.
    pub fn apply(&self, event: PointerInput) -> bool {
        if event.target_window == 0 || event.coordinate_width == 0 || event.coordinate_height == 0 {
            return false;
        }
        let window = NativeWindow(event.target_window as isize);
        if !is_process_window(window, ProcessId(unsafe { GetCurrentProcessId() })) {
            return false;
        }
        let Some(client) = client_size(window) else {
            return false;
        };
        if client.0 == 0 || client.1 == 0 {
            return false;
        }
        let x = scale_pointer_coordinate(event.x, event.coordinate_width, client.0);
        let y = scale_pointer_coordinate(event.y, event.coordinate_height, client.1);
        let key = (event.surface_id, event.pointer_id);

        match event.phase {
            PointerPhase::Move => post_move(window, x, y, event.buttons),
            PointerPhase::ButtonDown | PointerPhase::ButtonUp => {
                post_move(window, x, y, event.buttons);
                if let Some((down, up, high_word)) = button_messages(event.button) {
                    let message = if event.phase == PointerPhase::ButtonDown {
                        down
                    } else {
                        up
                    };
                    post(
                        window,
                        message,
                        mouse_key_state(event.buttons) | (usize::from(high_word) << 16),
                        client_point(x, y),
                    );
                }
                self.remember_buttons(key, event.buttons);
            }
            PointerPhase::Wheel => {
                post_move(window, x, y, event.buttons);
                let mut screen = POINT { x, y };
                if unsafe { ClientToScreen(window.0 as _, &raw mut screen) } == 0 {
                    return false;
                }
                let position = client_point(screen.x, screen.y);
                post_wheel(
                    window,
                    WM_MOUSEWHEEL,
                    event.wheel_vertical,
                    event.buttons,
                    position,
                );
                post_wheel(
                    window,
                    WM_MOUSEHWHEEL,
                    event.wheel_horizontal,
                    event.buttons,
                    position,
                );
                self.remember_buttons(key, event.buttons);
            }
            PointerPhase::Cancel => {
                let mut buttons = self
                    .pointer_buttons
                    .lock()
                    .remove(&key)
                    .unwrap_or(event.buttons);
                for button in 1..=5 {
                    let bit = 1 << (button - 1);
                    if buttons & bit == 0 {
                        continue;
                    }
                    buttons &= !bit;
                    if let Some((_, up, high_word)) = button_messages(button) {
                        post(
                            window,
                            up,
                            mouse_key_state(buttons) | (usize::from(high_word) << 16),
                            client_point(x, y),
                        );
                    }
                }
            }
        }
        true
    }

    fn remember_buttons(&self, key: (u64, u32), buttons: u32) {
        let mut state = self.pointer_buttons.lock();
        if buttons == 0 {
            state.remove(&key);
        } else {
            state.insert(key, buttons);
        }
    }
}

fn post_move(window: NativeWindow, x: i32, y: i32, buttons: u32) {
    post(
        window,
        WM_MOUSEMOVE,
        mouse_key_state(buttons),
        client_point(x, y),
    );
}

fn post_wheel(window: NativeWindow, message: u32, delta: i32, buttons: u32, point: isize) {
    if delta == 0 {
        return;
    }
    let delta = delta.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    post(
        window,
        message,
        mouse_key_state(buttons) | (usize::from(delta as u16) << 16),
        point,
    );
}

fn post(window: NativeWindow, message: u32, wparam: WPARAM, lparam: isize) {
    unsafe { PostMessageW(window.0 as _, message, wparam, lparam) };
}

fn client_point(x: i32, y: i32) -> isize {
    ((y as u16 as u32) << 16 | x as u16 as u32) as isize
}

fn mouse_key_state(buttons: u32) -> usize {
    let mut state = 0;
    for (index, flag) in [MK_LBUTTON, MK_RBUTTON, MK_MBUTTON, MK_XBUTTON1, MK_XBUTTON2]
        .into_iter()
        .enumerate()
    {
        if buttons & (1 << index) != 0 {
            state |= flag as usize;
        }
    }
    state
}

fn button_messages(button: u32) -> Option<(u32, u32, u16)> {
    Some(match button {
        1 => (WM_LBUTTONDOWN, WM_LBUTTONUP, 0),
        2 => (WM_RBUTTONDOWN, WM_RBUTTONUP, 0),
        3 => (WM_MBUTTONDOWN, WM_MBUTTONUP, 0),
        4 => (WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1),
        5 => (WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON2),
        _ => return None,
    })
}

fn scale_pointer_coordinate(value: i32, source: u32, destination: u32) -> i32 {
    if source == 0 || destination == 0 {
        return 0;
    }
    (f64::from(value) * f64::from(destination) / f64::from(source))
        .round()
        .clamp(0.0, f64::from(destination.saturating_sub(1))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(phase: PointerPhase, button: u32, buttons: u32, window: isize) -> PointerInput {
        PointerInput {
            surface_id: 1,
            target_window: window as i64,
            frame_number: 1,
            source: snowglass_input::mouse::PointerSource::Mouse,
            phase,
            pointer_id: 0,
            x: 20,
            y: 30,
            coordinate_width: 200,
            coordinate_height: 100,
            button,
            buttons,
            wheel_horizontal: 0,
            wheel_vertical: 0,
            timestamp: 0,
            sequence: 1,
        }
    }

    #[test]
    fn presentation_coordinates_scale_and_clamp_to_the_current_client() {
        assert_eq!(scale_pointer_coordinate(320, 1280, 1920), 480);
        assert_eq!(scale_pointer_coordinate(552, 720, 1080), 828);
        assert_eq!(scale_pointer_coordinate(-10, 1280, 1920), 0);
        assert_eq!(scale_pointer_coordinate(1280, 1280, 1920), 1919);
    }

    #[test]
    fn button_bits_become_win32_mouse_key_state() {
        assert_eq!(mouse_key_state(0b1), MK_LBUTTON as usize);
        assert_eq!(mouse_key_state(0b110), (MK_RBUTTON | MK_MBUTTON) as usize);
        assert_eq!(
            mouse_key_state(0b1_1000),
            (MK_XBUTTON1 | MK_XBUTTON2) as usize
        );
    }

    #[test]
    fn extra_buttons_use_the_xbutton_high_word() {
        assert_eq!(button_messages(4), Some((WM_XBUTTONDOWN, WM_XBUTTONUP, 1)));
        assert_eq!(button_messages(5), Some((WM_XBUTTONDOWN, WM_XBUTTONUP, 2)));
        assert_eq!(button_messages(6), None);
    }

    #[test]
    fn coordinates_are_packed_as_signed_sixteen_bit_halves() {
        assert_eq!(client_point(-2, 3) as u32, 0x0003_fffe);
    }

    #[test]
    fn posts_move_and_button_messages_to_an_owned_window() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, MSG, PM_REMOVE, PeekMessageW,
        };

        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                200,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!window.is_null());

        WindowMessageForwarder::default().apply(pointer(
            PointerPhase::ButtonDown,
            1,
            1,
            window as isize,
        ));
        let size = client_size(NativeWindow(window as isize)).expect("test window has a client");
        let expected = client_point(
            scale_pointer_coordinate(20, 200, size.0),
            scale_pointer_coordinate(30, 100, size.1),
        );

        let mut message = MSG::default();
        assert_ne!(
            unsafe { PeekMessageW(&raw mut message, window, 0x0200, 0x020e, PM_REMOVE) },
            0
        );
        assert_eq!(message.message, WM_MOUSEMOVE);
        assert_eq!(message.wParam, MK_LBUTTON as usize);
        assert_eq!(message.lParam, expected);
        assert_ne!(
            unsafe { PeekMessageW(&raw mut message, window, 0x0200, 0x020e, PM_REMOVE) },
            0
        );
        assert_eq!(message.message, WM_LBUTTONDOWN);
        assert_eq!(message.wParam, MK_LBUTTON as usize);
        assert_eq!(message.lParam, expected);

        assert_ne!(unsafe { DestroyWindow(window) }, 0);
    }
}
