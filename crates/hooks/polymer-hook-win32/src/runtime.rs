//! Win32 pointer delivery and virtual cursor state.

use polymer_hook::config;
use polymer_hook_input_win32::Runtime as InputRuntime;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::{
    Foundation::POINT,
    UI::Input::{HRAWINPUT, RAWINPUTDEVICE},
};

pub(crate) struct Runtime {
    logged_cursor_poll: AtomicBool,
    input: Arc<InputRuntime>,
}

impl Default for Runtime {
    fn default() -> Self {
        polymer_hook::telemetry::init_file_tracing(config::log_file(config::WIN32_LOG_FILE));
        Self {
            logged_cursor_poll: AtomicBool::new(false),
            input: Arc::new(InputRuntime::new()),
        }
    }
}

impl Runtime {
    pub(crate) fn start_input_receiver(self: &Arc<Self>) {
        let Ok(session) = config::session() else {
            return;
        };
        self.input.start_receiver(format!("{session}-input"));
    }
}

pub(crate) unsafe fn get_cursor_pos(
    point: *mut POINT,
    mut next: crate::hooks::GetCursorPosContext<'_>,
) -> i32 {
    if !point.is_null()
        && let Some((x, y)) = next.state.input.cursor_position()
    {
        if !next.state.logged_cursor_poll.swap(true, Ordering::AcqRel) {
            tracing::debug!("polymer: target first polled forwarded cursor position=({x}, {y})");
        }
        unsafe { *point = POINT { x, y } };
        return 1;
    }
    polymer_hook::next!(next, point)
}

pub(crate) unsafe fn register_raw_input_devices(
    devices: *const RAWINPUTDEVICE,
    count: u32,
    size: u32,
    mut next: crate::hooks::RegisterRawInputDevicesContext<'_>,
) -> i32 {
    let state = next.state.clone();
    let registered = polymer_hook::next!(next, devices, count, size);
    if registered != 0 && !devices.is_null() && size as usize == size_of::<RAWINPUTDEVICE>() {
        let devices = unsafe { std::slice::from_raw_parts(devices, count as usize) };
        state.input.raw_keyboard().register(devices);
    }
    registered
}

pub(crate) unsafe fn get_raw_input_data(
    input: HRAWINPUT,
    command: u32,
    data: *mut core::ffi::c_void,
    size: *mut u32,
    header_size: u32,
    mut next: crate::hooks::GetRawInputDataContext<'_>,
) -> u32 {
    if let Some(result) = unsafe {
        next.state
            .input
            .raw_keyboard()
            .data(input, command, data, size, header_size)
    } {
        return result;
    }
    polymer_hook::next!(next, input, command, data, size, header_size)
}
