//! Win32 pointer delivery and virtual cursor state.

use snowglass_hook::config;
use snowglass_input_win32::Runtime as InputRuntime;
use std::{
    fs::OpenOptions,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows_sys::Win32::Foundation::POINT;

pub(crate) struct Runtime {
    logged_cursor_poll: AtomicBool,
    input: Arc<InputRuntime>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            logged_cursor_poll: AtomicBool::new(false),
            input: Arc::new(InputRuntime::new(input_log)),
        }
    }
}

fn win32_log(message: impl std::fmt::Display) {
    let Some(path) = config::win32_log_file() else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

fn input_log(message: &str) {
    win32_log(message);
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
            win32_log(format!(
                "snowglass: target first polled forwarded cursor position=({x}, {y})"
            ));
        }
        unsafe { *point = POINT { x, y } };
        return 1;
    }
    snowglass_hook::next!(next, point)
}
