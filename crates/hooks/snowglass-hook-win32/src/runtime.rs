//! Win32 pointer delivery and virtual cursor state.

use snowglass_hook::config;
use snowglass_input_win32::Runtime as InputRuntime;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::Foundation::POINT;

pub(crate) struct Runtime {
    logged_cursor_poll: AtomicBool,
    input: Arc<InputRuntime>,
}

impl Default for Runtime {
    fn default() -> Self {
        snowglass_hook::telemetry::init_file_tracing(config::win32_log_file());
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
            tracing::debug!("snowglass: target first polled forwarded cursor position=({x}, {y})");
        }
        unsafe { *point = POINT { x, y } };
        return 1;
    }
    snowglass_hook::next!(next, point)
}
